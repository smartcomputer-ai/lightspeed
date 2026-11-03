# P194 — Harness core hardening

**Status:** In progress on `harness-core-hardening`. The owner delegated every
call on this branch and reviews it afterwards; commits can be reverted one at a
time. Greenfield: no compatibility shims, no legacy decoding, no migration of
old data.

## Goal

Very long-running agents: runs with thousands of turns, no default limits, and
growth pruned by design. A core the owner can trust: sessions do not brick, and
a failure becomes a normal failed run carrying its real message. Decoding is
lenient, and the reducer enforces structure rather than policy. Simple solutions
win over thorough ones. Bots are still work in progress, so only real wedges get
fixed there, and cheaply.

## Package order

Each package lands as its own commits and appends to Progress below.

1. `core-commit-safety` — dry-run appends, keep the reducer structural, freeze
   toolsets during a turn.
2. `session-liveness-read` — read sessions from the store plus describe; split
   `last_error` into `fatal_error` and `last_warning`.
3. `core-failrun` — FailRun funnel, close open tool groups, materialize steering
   on failure, typed caller errors.
4. `dead-surface-removal` — delete dead durable surface and internal dead code,
   keep forks and clones.
5. `forks-and-kind-constants` — revalidate the fork point under locks; one
   constant per event kind.
6. `host-loop-boundaries` — commit resume results in their arm, three-way
   rollover, prompt transport work, cancel queue.
7. `tool-dispatch-per-call` — park once per dispatch, per-call path with a unit
   activity for the residue, one promise numbering rule.
8. `promises-and-awaits` — truthful await and join results, batch-scoped
   approvals, payload-prepared follow-ups, recovery poll.
9. `store-rejections` — permanent errors stop retrying, data rejections fail the
   run, NUL handling.
10. `typed-blob-refs` — raw emission decode, validated `BlobRef`, typed blob
    roots, scope joins.
11. `run-limits-and-failure-kinds` — enforce `maxTurns` only when set, drop
    `maxToolRounds`, final failure kinds with real messages.
12. `state-pruning` — bound reduced state by design, serve old runs from the log,
    count only in-flight workflow-tool invocations.
13. `llm-provider-errors` — classify Anthropic overflow, one transport classifier,
    honest parse errors, provider constants home.
14. `compaction-triggers` — failure backoff, run-bound proactive compaction,
    entry-count trigger and structural guard.
15. `run-capacity-and-model-switch` — resolve input capacity per run on the
    accepted run, allow any model within a route.
16. `context-render` — render tool groups adjacent regardless of arrival order.
17. `api-waiters` — correlate waiters by token, typed pending and already-terminal
    results, run failure views.
18. `config-admission` — degrade disabled MCP at preparation, one setup validation
    at start, default-environment fill.
19. `mcp-native-default` — bound native output, keep the last tool list, move
    transport facts out of durable state, Native by default.
20. `bot-delivery-wedges` — typed start results, carried retries, generation map
    pruning, retention copy.
21. `oversized-input-refusal` — refuse an entry that can never fit and name the
    largest entry on compaction failure.
22. `session-restart` — explicit restart from the log.
23. `bots-dead-session` — fail the delivery and roll forward on a dead session;
    force close notifies.
24. `release-contract` — new namespace for breaking releases, SIGTERM, bot
    generation seeding, compatibility diff script.
25. `process-and-filesystem-tools` — atomic `apply_patch`, VFS versioned writes,
    `run_process` handle guarantees.
26. `reaper-and-retention` — lightweight reaper with honest reporting, retention
    cursor, sub-agent activity bounds.
27. `rename-wire-identities` — rename identities and lifecycle vocabulary on the
    wire and in durable events.
28. `rename-vocabulary` — reserve emission for the envelope; give native, managed,
    notify and revision one meaning each.
29. `rename-casing` — closed enums and crate-wide camelCase across the public API.
30. `api-reference-corrections` — correct stale reference text and reject unknown
    `session/list` params.
31. `format-guards` — lenient decoding rules, committed log and state shape tests,
    checkpoint bump.
32. `docs-and-review-notes` — correct guides and the upgrade doc, finish the
    hardening review notes.

## Risky calls for review

Later packages add their own entries.

- **Per-call dispatch reverses "do not split batches".** Reverses the September
  stance at the owner's request. A spawn beside an approval-gated call now waits
  for the decision. The dispatch package can be reverted, but later packages
  build on it.
- **FailRun on store data rejections.** A rejected event is discarded and the run
  fails as Internal instead of the workflow ending. A real store bug costs one run
  and loses the failed-workflow signal; the only alert is an error log.
- **The 2,000-entry guard.** A planner limit that fails runs once compaction cannot
  get below it, to stop sessions becoming unloadable near 3,000 entries. It is
  technically a limit, against "no default limits". Enabled sessions try compaction
  once per run first, ignoring the failure floor, so a temporary outage heals.
  Explicitly Disabled sessions fail every run past 2,000 entries until someone
  enables or runs compaction, and on a bot session nobody will. Its own commit, so
  it can be reverted alone.
- **Pruning windows of 256 runs, 256 promises and 64 steering batches.** A retried
  submission more than 256 runs later starts a new run, a steer retried past 64
  batches injects twice, an await on an old promise gets "no longer retained", and
  a duplicate approval decide after pruning gets `not_found`. Older run pages read
  `session_events` with no dedicated index, so deep paging on a very large session
  walks its log.
- **Bot conversation reset at breaking releases.** Undelivered bot events, dedupe
  tails, daily budgets, group `/activation` and conversation context are lost, and
  old open main rows stay open until closed. Drain first.
- **The 15-minute bot sweep and removal of the 24 h timer.** An event reaching a
  dead session with no run started rolls forward and is retried once on the new
  generation, so that conversation restarts without context, and an operator
  closing a main session on purpose also rolls forward at the next event. A lane
  already waiting detects death within 15 minutes, costing about 1,000 history
  events a day while a lane blocks continue-as-new. With the 24 h timer gone, a
  session stuck Running holds its lane until an operator force-closes it.
- **Restart scope.** Only on explicit request; run start never auto-restarts. A
  bound receiver's reply sent during an outage is lost, and only a recent emission
  window is re-sent. Workflow id reuse is unproven until the restart live suite runs.
- **Live MCP records with no digest pin.** An admin's URL or auth change applies at
  once, including to an already approved call.
- **Native MCP default.** New servers default to the runtime path, so traffic moves
  there. Provider-hosted at-least-once is accepted and documented, not fixed.
- **The breaking release itself.** A new Temporal namespace and purged databases are
  required, including fresh state on any existing AKS deployment. No `patched()`
  markers. The ls-benchmark fixture outside this repo needs a follow-up.
- **Unsigned documentation.** Rust reference text ships with its code, and all guide
  and upgrade wording sits in one final commit for review. The root README is not
  edited; wanted wording is listed in the review notes.
- **The sub-agent default deadline.** `SUBAGENT_DEFAULT_DEADLINE_MS` (1 hour, 24 h
  ceiling) is a default limit against the north star, and no decision covered it.
  Behaviour is left unchanged; this needs an owner decision.
- **Run detail tails on very long runs.** Very long runs return the opening events
  plus a tail with `earlierEventsOmitted` rather than the whole run. Paged run
  detail is a follow-up.
- **Admission refusals no longer end the workflow.** A `DomainError` at the admission
  site now rejects that one command and logs at error level. A real proposal bug
  costs one command and no longer produces a failed workflow signal. The two commands
  the workflow reissues from durable state, `ResumeToolBatch` and `ForceCancelRun`,
  are the exception: rejecting them would repeat forever, so a refusal takes the drive
  failure path, which since `core-failrun` fails the run.
- **Reducer trust moved to the proposal layer.** With policy out of the reducer, the
  dry run no longer catches a proposal builder that skips a moved check: a builder
  bug can now commit a policy-violating event that replays fine. Moved checks that
  held by construction have no second line of defence.
- **A queued message during a long parked batch delays rollover.** The toolset of a
  run prepared while a batch is parked stays pending, and `pending_toolsets` blocks
  continue-as-new. History only grows by what the park itself produces, but a run
  parked for days holds the rollover until the batch ends.

- **A dead session looks busy to bots until `bot-delivery-wedges`.** `session/read` used
  to fail on a recorded fatal error, which ended a bot delivery as `run_failed` after its
  attempts. It now succeeds with status `error`, and `session_status_from_view` still
  maps that to `Busy { run_id: None }`, so a lane on a dead session waits instead. The
  `BotSessionStatus::Failed` mapping is in that package, as instructed. Do not ship
  between the two.
- **A failed describe leaves `session/read` without the check.** The read is served from
  the store, so a Temporal outage logs a warning and reports the stored status rather
  than failing. A session can read as healthy during an outage, and each read of an open
  session first waits out the client's retries, about 10 s by its defaults (read from the
  client source, not timed). Mutations still fail.

- **FailRun absorbs real harness bugs.** Any `Domain` or `Planning` refusal with an
  active run costs that run and leaves the workflow running. A bug that refuses every
  run fails them all one at a time with no failed-workflow signal, and the only alert is
  the `session_step_refused` error log. The kind is `Internal`, so runs can be counted.
- **Error kinds callers see changed.** The one mapper turns many refusals that used to
  be `rejected` into `invalid_request`, `conflict` or `internal`. In-repo branches on
  `rejected` were read: the bot paths and the sub-agent paths still behave, and the
  live assertions on `rejected` are about lifecycle and duplicate submissions, which
  stay `rejected`. External clients branching on the old kind would notice. No live
  suite but the one below has run against this.
- **Run admission now checks the session's own tool choice.** A session whose choice
  dangles refuses every new run at the door, with a clear error, until the config or
  toolset changes. Before, it failed at the first turn. A queued run admitted against
  an older toolset still reaches FailRun.

- **Dead surface removal breaks replay and stored state until the purge.** Removing the
  rollover patch makes every dev Temporal history that carries its marker fail replay.
  `RunRecord.submission_digest` is now required, so a stored checkpoint holding `null` no longer
  decodes. The load rejects it with a warning and replays the log, so that costs a slow load rather
  than a stuck session, and the checkpoint constant is only bumped at the end of the branch in
  `format-guards`. Purge dev Temporal histories and sessions before running this branch against
  existing state.
- **A duplicate code tool result now rejects instead of no-ops.** The host drops results for
  non-pending calls, so nothing known reaches the reducer's rejection. If something ever does,
  `ResumeCodeToolCall` goes through `apply_internal` and fails the workflow rather than the call.
  The earlier digest made an identical joined redelivery a no-op. The review traced this: the resume
  loop only takes waiting calls, runs on the session loop with nothing else changing their status
  across its activity awaits, and one call's resume cannot finish another. A different result was
  already refused this way before the change.
- **The fork lock path has not run against Postgres yet.** The Docker stack was down
  during `forks-and-kind-constants`, and starting it was out of bounds, so
  `pg_live_fork_point_is_judged_under_the_source_lock` and the rewritten fork path only
  compile until the checkpoint. No production caller reaches the fork path. The one
  production change is `read_local_segment`, which every session read uses: it moved to
  a free function with the same SQL and binds.

- **The session loop changes in `host-loop-boundaries` have never run against Temporal.** Docker
  was down, so the five items are checked by unit tests and compilation only. The arm-local
  commits, the yields and the cancel queue change the order of workflow commands on the main path
  of every run, and only a live suite exercises that. Run `sessions_live` and
  `workflow_tool_plugins_live` at the next checkpoint before trusting this package.
- **A failed execution cancel is not asked again.** The old rescan re-issued every cancel after each
  rollover, which is what H-27 removes, and it also happened to retry a cancel that had failed. The
  activity now has three attempts in 90 s, then a warning, and that execution runs on until it ends
  by itself. Accepted because the old options retried forever and held up the whole session loop.
- **A cancel that lands with a finished generation now loses the race to it.** Finished work goes
  first, so the result is committed and the cancel lands one event later. An answer that had already
  come back is kept in the transcript of a cancelled run, and the tokens are counted.
- **The stall guard adds a timer to history when it fires.** After 32 passes with the same clock and
  history length, the loop logs `session_loop_stalled` and waits one second. Its decision reads
  `history_length`, which assumes replay sees the same value per activation, as the rollover check
  already does. It has not fired in any run, so that assumption and the 32 are both untested. Nothing
  known reaches it today.
- **`tool-dispatch-per-call` changes which activity runs which call, and it has not run against
  Temporal.** Docker was down. Every batch with an `await` or a workflow tool now takes two activity
  paths, and this reverses the September call not to split batches, though the batch stays one logical
  batch. Worker loss now fails only the affected call. A spawn beside a gated call now waits for the
  decision, where it used to run first. Consecutive parallel-safe calls in these batches now overlap.
  Run `mcp_live`, `subagents_live`, `workflow_tool_plugins_live` and the code suites at the next
  checkpoint before trusting this package.
- **A provider approval pending beside a gated native call fails the run.** `RunParked` refuses that mix,
  and the dry run now catches it, so it ends as a normal failed run with the reducer's message. Whether a
  provider ever sends `function_call` and `mcp_approval_request` in one response is unverified, so I left
  the refusal in place rather than relax a check on a shape nobody has seen.
- **The recovery poll treats some query errors as final from reading alone.** A rejected
  query, a payload conversion failure and `InvalidArgument` fail the promise; everything
  else stays Pending. That a failed or missing handler returns `InvalidArgument` and that
  no poller returns `FailedPrecondition` come from reading and recall, not from a server.
  `workflow_tool_start_recovery_query_meets_the_reply_schema` has never run. If the
  mapping is wrong, a broken handler waits until the deadline or the run ends, and with no
  deadline it polls every 60 s for as long as the run lasts.
- **A detached follow-up can stall the session until `store-rejections`.** The follow-up now
  reads its payload through `materialize_await_result` on `activity_options()`, which has no
  retry bound, so an unreadable payload retries forever and blocks the outer loop, as an await
  of that promise does. Only a malformed ref gets this far: a well-formed ref to a missing blob
  already stalls at `append_events` with `MissingBlobs` when the promise resolves, before this
  change too. Before, a malformed ref failed each generation and the session stayed usable. The
  ingest gate in that package closes this, after which only a store outage blocks here, and
  waiting that out is correct. Do not ship between the two packages.
- **File attachments do not reach a detached follow-up.** A run input has no field for
  them, so a detached sub-agent's linked file is named in the result JSON only. It is not
  recorded on a tool completion, so the session's own final answer cannot link it onward by
  handle as it could after an await. Images do arrive as media entries.

## Progress

### `core-commit-safety` (H-01, H-16, H-11, H-06)

Landed:

- `c59fd9b3` starts this document.
- `db961ebe` (H-01) dry-runs every append. `append_action` reduces the encoded batch
  on a cloned state, through the same decode and apply path as `resume_appended`,
  and returns the refusal before `AppendEvents`. The detach branch skips promises
  cancelled in the same result, which removes the model-reachable
  `[Cancelled, Detached]` trigger.
- `964c13ee` (H-16) keeps policy out of the reducer. Arms enforce position, ids,
  revisions and references; each moved check still runs at admission, in proposal
  building, or holds by construction. `CompactionRequested` checks non-empty, every
  id active, none unconsumed run input, plus boundary, trigger and run id, and makes
  no prefix or ordering claim. The prefix computation stays in the planner.
- `fa6c40a9` (H-11) requires a steering batch to be fully materialized before a turn
  can consume it, so a steer admitted in the `Turn::Started` window is delivered.
- `846b1eca` (H-06) adds `harness::tools_frozen`: an active turn, a completed
  tool-call turn with no batch, or an open batch including a parked one. It gates
  `publish_pending_tools`, `publish_tools`, the `PreparedRun` branch and the wait
  loop's pending-toolset clause. Admission rejects `PatchTools` and `ReplaceTools`
  with `ActiveWork` while it holds.

Checks: `cargo test -p harness` (272 passed), `cargo test -p temporal-workflow --lib`
(161 passed, run for the new wait-loop test), `cargo check --workspace --all-targets
--locked`. No live suite ran; the Temporal paths wait for the first checkpoint.

Calls made:

- A refusal at the admission site becomes a rejected admission, kind
  `InvariantViolation`, logged at error level (`session_admission_refused`). This
  also covers `DomainError`s from `admit_command` itself, which used to end the
  workflow. `last_warning` is added by `session-liveness-read`.
- A refusal on the drive path still fails the workflow, as instructed, until the
  FailRun funnel in `core-failrun`. The log stays loadable and force close works.
- The dry run decodes the encoded events rather than applying the typed proposals,
  so an event that would not decode is refused too. Measured earlier at about 35 ms
  on a 1.8 MB state in an unoptimized build.
- The H-16 audit removed more than the listed checks, on the same rule: canonical
  invocation and execution id recomputation, the completion promise map shape and
  promise source comparison on `Emitted`, the managed declaration version and
  creation fingerprint recomputation, turn outcome versus generation result,
  approval note rules, the runtime-owned promise shape on `Created` and the ownership
  check on `Detached`, replaceable-kind rules on `EntriesReplaced`, external edit
  rules on `KeyPrefixReplaced`, the tool-choice reference on recorded configs and
  runs, and the code scope grant, id and joined-deadline checks.
- Kept in the reducer: promise terminal transitions, the provider and API-kind pin on
  recorded config updates and run overrides, the idle check on config updates, and
  the code scope toolset revision check, which `tools_frozen` makes unreachable for
  new input.
- `has_unstarted_tool_call_turn` looks only at the latest turn now. No later turn can
  start before that turn's batch exists, and the old scan was quadratic on long runs.
- The unreachable revision branch in `planned_tool_for_turn` is deleted, along with
  the no-op `validate_removal_reason`, `validate_rewrite_reason` and
  `validate_active_context_api_kind`.

Deviations:

- `read_tool_emissions` still re-validates bindings and fingerprints on read. It is a
  projection rather than the reducer, and a tightened rule fails only that read, not
  the session. Left as it is; the same rule would argue for trimming it later.
- The `tooling::plan_next` revision error stays. `tools_frozen` makes it unreachable,
  and the reducer's `BatchStarted` revision check stays with it.
- The separate `Run::Failed` conversion, the duplicate id checks in
  `generation_result_proposals` and the second-gated-call fix are not here. They
  belong to `core-failrun` and `tool-dispatch-per-call`.

Review of this package, `1ad8fdad`:

- The review found one bug the package introduced. Rejecting every reducer refusal at
  the admission site looped for `ResumeToolBatch` and `ForceCancelRun`, which the
  workflow issues again from durable state. These two now keep the drive's failure
  path, as they did before the package. The log stays loadable because the dry run
  wrote nothing.
- Each moved H-16 check was traced to admission, proposal building or construction,
  and no gap was found. The H-01, H-11 and H-06 behaviour matched the briefs.
- Checks: `cargo test -p harness --lib` (272 passed), `cargo test -p temporal-workflow
  --lib` (162 passed), `cargo test -p temporal-runtime --lib` (469 passed, 1 ignored),
  `cargo check --workspace --all-targets --locked`. No live suite ran.

Handoff to `core-failrun`:

- The funnel is specified for the `core_drive` error site, but refusals also fail the
  workflow at the `tool_batch_resume` and `cancellation_watchdog` sites in the session
  loop. Both run `drive_until_idle`, so planner refusals can surface there too. The
  funnel should cover them, or these failures stay workflow-fatal.

Open, not introduced here: `resume_tool_batch_proposals` maps every `DomainError` to a
rejection, and `process_pending_tool_batch_resumes` records it and carries on. A
proposal-level rejection that is deterministic, rather than a stale claim, re-runs the
await materialization on every pass with no backoff. The rejection kinds do not tell
the two apart today. Closed by the `core-failrun` review.

### `session-liveness-read` (H-61)

Landed:

- `aa788978`: `last_error` on the session status is split. `fatal_error` is set only by
  `record_error` and `record_bootstrap_error`, every caller of which returns the error
  straight after, and it is the only field the gateway waiters and
  `wait_for_closed_session` fail fast on. `last_warning` takes the advisory setters and
  logs. No gateway path reads it, and only a rollover drops it. The clears on append, on
  admission failure and in bootstrap are deleted. The bot controller's and the
  environment job's own `last_error` are untouched. The stale-flag force terminate on
  close goes with this.
- `aee00c0d`: `session/read` no longer queries the workflow. It projects from the store
  and, for rows that are not closed, calls `execution_liveness`. A stopped execution
  reports the existing `error` status and `SessionView.execution_failure` (`failed`,
  `terminated`, `timedOut`, `missing`). Lists stay store-only. A completed or canceled
  execution under an open row maps to `terminated`. The web detail page shows a banner
  and a Stopped chip. Public API contract and TypeScript consumers are regenerated. The
  workflow contract did not change.

Calls made:

- `execution_liveness` is the single helper. `SessionLifecycleIo::is_running` and
  `classify_workflow_interaction_not_found` use it, and `workflow_is_running` is deleted.
  `ExecutionLiveness` is re-exported from `gateway::service` for the later packages;
  `bots` will need a further re-export through `gateway`.
- A closing workflow commits the close and then completes, so a dead execution behind an
  open row can mean the row was read too early. The read projects once more on that path
  only, and reports the failure only if the row is still open.
- A row in lifecycle `New` is treated as open when its execution ran and stopped. With no
  execution at all it stays `notLoaded`, because a sub-agent's row is written before its
  workflow starts. See the review follow-up below.
- Statuses that do not say the execution ended count as running: `ContinuedAsNew`,
  `Paused`, `Unspecified`, `Unknown` and anything newer. A paused workflow used to count
  as not running, so force close now signals it first and falls back to terminate when
  the close does not land.
- `queue_emission` returns its refusal, because the state method has no replay flag and
  warnings log only outside replay. The dead promise-id branch stays as advisory, as
  instructed.
- The composer stays enabled on a dead session. Restart from the log comes with H-64, and
  the banner already says new messages will fail.

Deviations:

- `classify_workflow_interaction_not_found` changed beyond a move to the helper. Using it
  for reads with no row would have turned a deleted session, whose execution completed,
  into `session_bootstrap_failed`, and bots would have lost `Missing`. With no row, it now
  returns the store's not found unless the execution failed. A read of an unknown id keeps
  the message `session not found: <id>`.
- The two fact-checks in the brief disagree on the cancel activity's retries. The code
  has them bounded: `activity_options` sets 3 attempts and a 90 s schedule-to-close. So a
  retryable failure does reach the warning branch, and the corrected comment says the
  activity refused or used up its attempts. The execution is not asked again.
- The `session/read` method description is cut to fit the 360-byte manifest limit.

Checks: `cargo check --workspace --all-targets --locked`; `cargo test -p api -p
api-projection`; `cargo test -p temporal-workflow --lib` (163 passed); `cargo test -p
temporal-runtime --lib` (470 passed, 1 ignored); `npm run typecheck`; `SessionsPage.test`
(74 passed). The SDK needs `npm run build --workspace @lightspeed-ai/sdk` after
regeneration before the web typecheck sees new types.

Live, on the hardening database and namespace, both passed:

- New `temporal_live_session_read_needs_no_worker_and_reports_a_dead_execution`. It stops
  the worker first, then reads, terminates, reads, lists, force closes and reads again.
- `workflow_tool_reply_requires_exact_stored_producer`, updated for `last_warning`.

Other live suites wait for the checkpoint. A live fixture that opens a session row with
no workflow behind it and then reads it would now see `error` with `missing`. I found
none by reading, but did not run them all.

Handoff:

- `bot-delivery-wedges`: map `SessionStatus::Error` to `BotSessionStatus::Failed`, and
  reuse `execution_liveness` for the sweep. See Risky calls.
- Waiters still poll the status query, so they need a worker and a dead execution can hold
  them to `operation_timeout` when there is no worker to answer.
- Several main-loop exits fail the workflow without `record_error`:
  `prepare_initial_session`, `code_tools::process_pending`, `drive_from_state` before the
  admission drain, `drain_pending_admissions` and the code tool dispatcher. Those leave
  `fatal_error` empty, so waiters do not fail fast on them. This was already so with
  `last_error`. `core-failrun` or H-64 is the natural place to fix it.

Independent review follow-up:

- `07555062`: the note that a row without a live execution is real was wrong. A sub-agent's
  row is written before its workflow starts, so during that start, and while it keeps
  failing and retrying, `session/read` reported `error` with `missing` and the page said
  the workflow no longer exists. The bot sweep would have inherited this. A `New` row with
  no execution now stays `notLoaded`. A `New` row whose execution stopped still reports it.
  The classifier's decision table is now a pure function with a unit test, since only one
  row had a test, a live one.
- `048c00ab`: two descriptions corrected. `failed` pointed at "the log", which reads as
  the session log and does not hold the error. `last_warning` said only a newer warning
  replaces it, but a rollover drops it too. Contracts regenerated.
- `60b52956`: the detail page read the session only on load and on run events, and a
  workflow that dies mid-run emits none, so the banner never appeared for someone watching.
  The page re-reads every 15 s while the status is `active`.

Review checks: `cargo test -p temporal-runtime --lib` (472 passed, 1 ignored), which also
closes the gap of not re-running it after the classifier refinement; `cargo test -p
temporal-workflow --lib` (163 passed); `cargo test -p api -p api-projection`;
`SessionsPage.test` (75 passed); web typecheck; `cargo check --workspace --all-targets
--locked`. The contract exporters leave no diff. No live test was re-run for the
follow-up: the `New` row case is covered by unit tests only, so it waits for the checkpoint.

Reviewed and found sound: every `record_error` caller returns the error straight after, so
removing the clears cannot leave a sticky `fatal_error`. Warnings log only outside replay
and add no workflow commands. The workflow's only clean exit is closed and quiescent, so
`completed` under an open row does mean it stopped short, once the re-read has ruled out a
close in flight.

### `core-failrun` (H-35, H-09, H-43, H-42, H-94, H-60)

Landed:

- `a64527c8`, `f1bcc9e4` (H-35): `CommandRejectionKind` becomes `SessionLifecycle`,
  `ActiveWork`, `MissingActiveRun`, `UnknownReference`, `Invalid`,
  `ProviderCompatibility`, `DuplicateSubmission`, `RevisionConflict`, `Conflict`, `Refused`,
  `Internal`. `command_rejection_to_api_error` is the one mapper, reached through
  `AgentAdmissionFailure::api_error` and the candidate path. Rejection Display is the message
  alone, `DomainError::message` drops the reducer prefix from gateway text, and
  `is_profile_unapplicable` is now `kind == InvalidRequest`.
- `c589f64e` (H-09): Stop or Unknown with calls queues them. A tool-call finish with nothing
  to run fails the turn. Repeated call ids, or a call entry for no call, fail it too. Only a
  queued turn puts `ToolCall` entries in context. `open_tool_group_closing_entries` is the
  shared helper, used by `ForceCancelRun` and `FailRun`. The watchdog moved after the resume
  step.
- `9c118556` (H-43): `ApprovalEvent::RunUnparked`, public as `approvalRunUnparked`, emitted only
  when no approval remains and no batch is parked. The reducer's `Decided` arm no longer flips
  status. Stored and live activity follow the same park. `pending_approval_cancellations` serves
  `CancelRun`, force close and `FailRun`. A run ended with its batch open projects unfinished
  calls as cancelled.
- `97929610` (H-42): an empty queued compaction finishes Failed and stamps the manual revision;
  `compaction_safe_boundary` counts an unstarted tool-call turn; `next_context_compaction_request`
  needs an open session; force close ends a pending or queued compaction and passes the hold.
- `28749151` (H-94): `has_unmaterialized_steering`, one guard after the arms of
  `terminal_run_proposal`, so any failure kind but `Cancelled` inherits the hold. A failed
  context-limit compaction writes the steering in its own append. `runs.rs` wording updated.
- `da5977b5` (H-60): `CoreAgentCommand::FailRun`, `drive_past_refusals` at all three drive
  error sites, `CoreAgentDriveError::refusal_message`, the well-known
  `RUN_INTERNAL_FAILURE_CONTENT` blob, and early validation in `prepare_toolset` and
  `RequestRun`.

Checks: `cargo test -p harness` (305 passed), `cargo test -p temporal-workflow --lib` (166 passed),
`cargo test -p temporal-runtime --lib` (473 passed, 1 ignored), `cargo test -p api -p
api-projection`, `transcript.test.ts` (82 passed), web typecheck, `cargo check --workspace
--all-targets --locked`.

Live, on the hardening database and namespace, passed:
`temporal_live_harness_refusal_fails_the_run_and_the_session_serves_the_next`. It covers session
start and config put refusing a dangling tool choice, a real reducer refusal becoming a failed
`Internal` run with the refusal's text, and the next run completing on the same execution. The
fake LLM gained `with_refused_generations` for it. Other live suites wait for the checkpoint;
those that assert a `rejected` kind on input the mapper now calls invalid are the first to look at.

Calls made:

- The funnel also covers `tool_batch_resume` and `cancellation_watchdog`, per the earlier handoff.
  So a refused `ResumeToolBatch` or `ForceCancelRun` now fails the run rather than the workflow.
- `drive_past_refusals` keeps driving after a failure rather than returning to the wait loop.
  Otherwise a `close_on_terminal` session with nothing queued would never reach its auto-close.
- A cancelling run's steering stays dropped in `FailRun`. The spec lists the steering step
  without exception; the settled rule that cancel drops steering won.
- A direct `DomainError` in an `anyhow::Error` counts as a refusal, which covers
  `attach_promise_control_runtime`. `append_events` strips the type from everything after the
  append, so a post-commit reducer failure stays fatal.
- A recorded tool result that fails validation is replaced by a cancelled result in the closing
  entries. Such a result can be the refusal's cause, and repeating it would refuse FailRun too.
- Compaction refusals with no run reuse `resume_context_compaction` with a Failed result. No new
  command was needed.
- `next_context_compaction_request` now passes a domain refusal through unwrapped, so a provider
  compatibility failure keeps its text in the failure message.
- Reducer refusals at the admission site use kind `Internal`, which maps to an API internal error.
- `already terminal` on a decision uses `Conflict` until `api-waiters` adds its own kind. It reaches
  callers only through a race past the gateway's check.
- "No older compactable context" uses `Refused`, so it stays `rejected`. Compaction already pending
  uses `ActiveWork`. `validate_standalone_compaction_can_start` is gone; admission checks both.
- `AdmitSystemWorkflowTool` conflicts use `Invalid`, matching what `prepare_toolset` already answers.
- A plain close now refuses while a compaction is queued, not only while one is pending.
- A new well-known blob, `UNUSABLE_TOOL_CALLS_FAILURE_CONTENT`, names the bad tool-call turn
  instead of the generic boundary text.
- Every main-loop exit records `fatal_error` through `errors::fatal`, closing the handoff from
  `session-liveness-read`. The repeated error blocks in `session/mod.rs` collapsed into it.

Deviations:

- "Both outcome functions" is one: H-16 removed `validate_outcome_for_generation`.
- `generated_tool_calls_are_usable` does not require a `ToolCall` entry for every observed call.
  Many harness fixtures carry calls with no entries, and every real adapter writes them, so this
  direction stays unchecked. A call with no entry would produce a result with no call.
- The adapters' `finish_reason` mappings are untouched. The harness rule makes them harmless and
  this package's crate list excludes `llm-runtime`. The Responses failed-status branch still keeps
  its calls in the result; the harness drops them.
- Force close does not close the tool group, as the brief allows: the session ends.

Open:

- A hosted approval cancelled by `CancelRun` or `FailRun` leaves its `mcp_approval_request` entry in
  context with no response. This is not new. Whether OpenAI accepts that on the next request was
  not tested. The unit replay checks tool call pairing only.
- A run parked on both a batch and a hosted approval stays `waiting` in the stored projection until
  the batch resumes, while live activity says `working` once the approval is decided. This needs one
  response carrying both an await and a hosted approval, and heals at the resume.
- `FailRun` does not materialize run input that never entered context. That needs a refusal before
  the first context step, and the run's source still holds the input.
- A force close during a slow compaction is covered by unit tests only.

Review of this package:

- `0312eec0`: the funnel missed the wedge the earlier review had left open. Admission
  rejected every error from building a `ResumeToolBatch`, so a resume wrong on every attempt
  looped: the satisfied await issued it again each pass, running its materialize activity and
  growing history without bound, forever on an active run. Only an overtaken claim is rejected
  now, as a `Conflict`, and the host claims again. Everything else fails the run. This closes the
  open item under `core-commit-safety`. Covered by a harness test of the classification; the
  workflow's loop itself has no test short of a live one.
- `9224ccaa`: the steering hold let the context planner check the compaction threshold first. A
  session over it made a provider call between the failed turn and the failed run, which with
  the provider down could hold the failure back for a whole retry budget. Reproduced by a test
  before the fix.
- `cccb7aa0`: a refused call result returned out of the call group and dropped its in-flight
  siblings. Dropping an activity future does not cancel it, and the workflow now lives on, so
  those tools kept running for a failed run. They are cancelled like a preempting cancel. Read
  from the code; no test, since this path runs only inside a workflow.
- Traced and found sound: every `Decided` goes through the admission that adds `RunUnparked`;
  `FailRun` reduces from any run status and over a pending compaction; run termination closes
  promises and code tool scopes for `Failed` as it does for `ForceCancelled`; config put keeps
  publishing tools before the config, so moving the identical-document check cannot refuse a
  valid choice at setup; the contract exporters leave no diff; every adapter pairs `ToolCall`
  entries with observed calls one to one, so `generated_tool_calls_are_usable` cannot fail a
  healthy turn.

Open after review:

- A refusal on the code tool completion path (`code_tools::process_pending`) still fails the
  workflow rather than the run: it goes through `apply_internal`, outside the three funnel sites.
  It does not loop. Nothing known reaches it, so it was left rather than widening the funnel on
  speculation.
- `admission_failures` grows by one for each overtaken resume claim, as it does for any rejected
  command. The list has no bound.

### `dead-surface-removal` (H-26, H-44)

Landed:

- `9b0978e5` (H-26): deleted `ToolCallStatus::Observed`, `TurnOutcome::ContextUpdateRequired`,
  `ContextEvent::StateReplaced` with `ContextRewriteReason`, `ContextRemovalReason::Pruned`, its
  view and client handling, `CoreAgentJoins.correlation_id` and `CorrelationId`,
  `ActiveRun.output` and `failure`, `LlmRequest.provider_response_id`, `ToolCallExecutionPolicy`,
  `RunConfig.provider_params`, `ContextCompactionTask.params`, both `validate_provider_params`, the
  code tool digest, wire reader and redelivery branch, close without cancel and `cancel_requested`,
  the rollover patch, `legacy_max_steps_per_input`, `run_submissions`, `v1()`, the unreachable
  `initialized` return, `gap` and `EventLogGap` with their CLI and web branches, `activationText`,
  compaction aliases, alternate `api_kind` spellings, the `payload.kind` run boundary fallback, the
  media-only envelope, and three dead `api_projection` functions. Public API, workflow contract and
  TypeScript consumers regenerated.
- `606396df` (H-44): deleted the `CachedBlobStore` family, `read_tool_emissions` and its projection,
  `CoreAgentDrive.head` with the workflow's copy and `CreateOrLoadSessionResult.head`,
  `FsSessionStore`, `FsVfsCatalogStore`, `FsStore`, `ToolCallSummary`, the `ReplaceSessionConfig`
  admission arm, and the smaller items on the brief. The store trait's error-returning and
  checkpoint defaults are required. `sibling_tool_ids` replaces `sibling_calls`.

Calls:

- `submission_id` and `submission_digest` stay two fields on `RunRecord`, with the digest required
  and computed for every run, rather than one `Option<RunSubmission>`. The package reads
  `run.submission_id` directly, and a digest with no id is harmless.
- `ContextCompactionTask.params` goes as well as the run config field, since nothing populates it.
  `LlmRequest.params` stays as direct adapter input with its doc corrected.
- The four runner-driven OpenAI live suites use a test client wrapper,
  `unstored_openai_responses_client`, which sets `store` and `stream` to false.
- `CreateOrLoadSessionResult.head` goes too: with the workflow's copy gone it had no reader, and
  `core_state.reduced_to` carries the same position.
- `CheckpointReduction` keeps its struct with the one field, so the format change stays with the
  checkpoint constant bump in `format-guards`.
- `race_activity_with_admissions` keeps its fallback with a comment saying it is defensive, and the
  continuation `version` guard stays, as the brief rules.

Deviations:

- Not deleted, per the package ruling: the `RunFailureKind` variants, `StepLimitReached` with
  `steps_taken` and `next_action_unbounded`, `FsBlobStore`, and `read_all_session_entries`.
- Several items were already gone from earlier packages: `CommandRejectionKind::Other`,
  `validate_active_context_api_kind`, `let _ = config`, and the validators on removal reasons.
- `WorkflowWake` was left alone: the brief removes it only if the timer code is touched.
- The OpenAI Completions compaction live suite lost its `store: false`. The field was on the removed
  task params, which that adapter never read, so behaviour does not change.

Verified: `cargo check --workspace --all-targets --locked`, `cargo test --lib` for `harness`,
`store-pg`, `store-fs`, `temporal-workflow` and `temporal-runtime`, `cargo test` for `api`,
`api-projection` and `test-support`, `npm run typecheck`, and the affected web and SDK tests. No
live test ran. The live suites only compile against the new client wrapper and the always-cancelling
close.

Open, for the docs package:

- `docs/documentation/integrating-and-extending/api-and-typescript.md` still checks `gap` in a
  sample and tells readers to check it.
- `docs/documentation/integrating-and-extending/workflow-tools.md` still names
  `read_tool_emissions`.
- `docs/roadmap/later/pNNN-local-runtime-without-temporal.md` and
  `docs/roadmap/p185-external-harness-sessions.md` describe `store-fs` as it was.

Review of this package:

- `df4e3ea0`: removing the workflow's head copy left `AppendSessionEventsResult.head` with no
  production reader, still crossing every append activity. It goes, and tests derive the position
  from the entries. The storage test's notify helper also moved below that test's imports.
- Traced and found sound: the widened host filter drops only completions, and a waiting call's
  resume takes the separate `reconcile_waits` path, so joined and await calls still resume. Production
  only ever closed scopes with cancel on, so `closed` and the old `cancel_requested` agreed on every
  path, including run termination. `apply_event` sets `reduced_to` on every entry, so the drive's
  fence equals the old head wherever the old head was right, and `validate_reduced_head` still guards
  the load boundary. A batch's siblings dedupe through a `BTreeSet`, which stays deterministic, and the
  call is excluded by position, so two calls to one selection tool still violate the batch rule. No
  lifecycle path reopens a closed session or fork, so the `closed_at` resets were vacuous. Every
  production `api_kind` source uses the canonical spelling. A completed run's id reads the same from
  the record as from the deleted map.
- Every item on both briefs and the package ruling is either deleted or recorded as deferred above.
  No source, test or durable doc added by this package cites a roadmap id. Besides the `gap` and
  `read_tool_emissions` guide mentions already listed, no durable doc names a deleted symbol.

Review checks: `cargo check --workspace --all-targets --locked`, with no warnings. `cargo test --lib`
for `harness` (303), `store-fs` (2), `store-pg` (12), `test-support` (13), `api` (103),
`api-projection` (45), `llm-runtime` (199), `temporal-workflow` (165) and `temporal-runtime` (473, 1
ignored). `cargo test -p api --tests` and `cargo test -p temporal-workflow --test workflow_contract`.
`npm run check:generated` and `npm run typecheck`. The workflow contract exporter and the TypeScript
generators leave no diff. No live test ran, so the `store_pg_live` and `session_lifecycle_live`
edits and the always-cancelling close only compile until the checkpoint.

### `forks-and-kind-constants` (H-81, H-46)

Landed:

- `bc422f59` (H-81): `create_forked_session` in `store-pg` now judges the fork point after
  it holds the root and source row locks. The long prefix is still read unlocked, so a
  long session's appends do not wait on it. The head comes from the locked row, and the
  tail since the first read is read inside the fork transaction, so a run appended in the
  gap is seen. `safe_fork_seq` reads the head once and the log through that head, which
  is a consistent snapshot because the log only grows. `read_all_effective_events` became
  `read_effective_events_through`, and `read_local_segment` became a free function that
  takes any executor. API docs, CLI help and Platform copy no longer describe history forks
  or config-only clones. The contract and TypeScript consumers are regenerated.
- `3d6ecfca` (H-46): `harness::event_kind` holds one constant for each of the 57 core event
  kinds. The codec's envelope arms and every reader use them, including `RunUnparked`. Tool
  ids that code branches on are constants too, in non-test code.

Calls made:

- A constant for all 57 kinds, not only those read outside the codec today. The package
  title asks for one per kind, and a partial set would leave the codec half literal.
- A module, `event_kind::RUN_STARTED`, rather than flat names like
  `CORE_AGENT_RUN_STARTED_EVENT_KIND`, which reach 70 characters in match arms. The two
  old lifecycle constants are removed so there is one convention.
- One fork validation under the locks instead of an extra unlocked check first. A check
  on stale entries could only refuse a fork that is valid by the time the locks are held.
- `SLEEP_TOOL_ID`, `MCP_FIND_TOOLS_TOOL_ID` and `MCP_CALL_TOOL_ID` were added beyond the
  package text. The brief's judge asks for ids harness branches on, and sleep sits in the
  same match arms as the other three concurrency ids.

Deviations:

- Platform copy (delete dialog, retention editor, settings sheet, deleted sessions, demo
  route, one backend comment) was reworded though the package names only Rust and CLI text.
  The owner's option says to reword text implying forks exist, and these are the strings
  people actually see.
- There are no kind literals in `store-pg` source or SQL to replace. It goes through
  `apply_lifecycle_projection`. The only non-codec literals left are two tests feeding a
  deliberately unknown kind. The codec tests and fixtures keep the literal strings as pins
  on the durable format.
- The old fork-order reproduction test is not ported. It showed that the shared helpers
  accept stale input, which stays true by design. The new live test covers the order
  instead, by landing an append while the fork waits on the lock.
- Effect kinds are untouched, as the brief rules.

Verified: `cargo check --workspace --all-targets --locked`, `cargo test -p harness` (305),
`cargo test -p store-pg --lib` (12), `cargo test -p tools --lib` (317), and the delete dialog's
web test. The workflow contract exporter leaves no diff. No live test ran, because Docker was
down; the attempt reached no database. See Risky calls.

Open:

- For the docs package: `sessions-and-runs.md`, `models-and-credentials.md`,
  `private-and-shared-work.md` and `deployment/operations.md` still mention forks. Root `README.md` line 131 lists them as
  shipped and needs the owner's permission to change.
- `session_events.event_kind` and its index have no reader in the Rust code. Left alone,
  because this package does not change the schema.

### `host-loop-boundaries` (H-07, H-08, H-02, H-14, H-27)

Landed:

- `2687bcbe` (H-07): the GenerateLlm, CompactContext and InvokeTools arms commit what their activity
  returned through `commit_resume`, and `invoke_tool_batch_as_unit` appends its own resume, including
  the boundary-failure branch. `action` at the loop top only holds planner output, so the drain and
  re-plan are unchanged and a steer still extends a run before its completion append.
- `6b4874fb` (H-08): every `select!` in the workflow crate is `select_biased!`, with the orders the
  package names. The other sites have a fixed order too: `wait_loop`, `lanes` and `controller` take
  the state wait first (their arms do the same thing either way), and the chat poll timer takes
  cancelled first. `clippy.toml` disallows `futures_util::select`.
- `df50d80d` (H-02): `history_boundary_outcome` is three-way over `outer_loop_can_clear_now`, and
  `drive_boundary_after_commit` sets the checkpoint flag. So does `drain_pending_admissions` when it
  took anything, which covers rejected admissions and no-op operations, and so do source resolutions.
  Stale start backoffs are dropped.
- `9a3e897f` (H-14): `drive_boundary_after_commit` also yields on `transport_work_due`, so a job or
  sub-agent starts before the next generation.
- `0d8632df` (H-27): `pending_execution_cancels` is filled per entry in `append_events`, carried
  across continue-as-new, and is all that `process_execution_cancels` reads. The rescan and
  `cancelled_workflow_executions` are gone.

Calls made:

- `drive_owed` in the outer loop is not in the package text. With yields now common, a run's terminal
  append with a notify intent yields, and the wake predicates then see nothing to drive, so auto-reject
  and close on terminal never ran. The same gap already existed at a rollover on that append, so a
  continued execution also owes the drive one pass. Unit tests do not reach this; it is inline in the
  loop.
- A yield from the resume or watchdog drive restarts the pass. Otherwise the same pass goes on to the
  main drive and starts a generation before the transport work it yielded for.
- `StallGuard` is included. The judge recommended it; the planned action does not name it. It is
  about 25 lines and covers wake predicates nobody has found yet, such as an active run whose drive
  returns Idle.
- `outer_loop_can_clear_now` leaves out due deadlines and the cancel queue, though
  `transport_work_due` has both. A deadline can be held behind a compaction, so counting it would
  yield from the top of the drive with nothing consumed, and spin. The cancel queue does not block a
  rollover, so clearing it clears nothing. Transport yields come only after a commit, so these two
  cannot spin there.
- The early return for `run_preparation` and `pending_toolsets` in `history_boundary_outcome` is gone,
  since the predicate gives the same answer. A control admission passing preparation now yields, and
  the outer loop drains it.
- The cancel activity uses `preparation::activity_options()`, as the brief asks for a bounded policy.
- Cancels are still queued for invocations whose start failed. A start reported as failed may still
  have reached the server, and the cancel costs one no-op.
- `clippy.toml` uses `allow-invalid`, so crates without `futures_util` do not warn. Checked by putting
  a `futures::select!` back in `temporal-workflow` and by a clean `cargo clippy -p bots`. It caught a
  plugin workflow in `workflow_tool_plugins_live.rs`, now biased with cancelled first.

Deviations:

- The method is `queue_execution_cancel_for_entry`, singular, because it takes one entry inside the
  apply loop. The package calls it `queue_execution_cancels_for_entries`.
- The brief's judge puts the cancel queue in `workflow_state_allows_continue_as_new` and the blockers.
  The package carries it across instead, so it is in neither. It is in the wake predicate,
  `transport_work_due` and `workflow_state_is_closed_and_quiescent`.
- There is no workflow test delivering a signal in the same activation as an activity result, because
  nothing here runs the workflow without Temporal and the timing cannot be forced live. The harness
  tests walk the new order instead, which does not prove the loop follows it.
- The field keeps the name `execution_has_rollover_checkpoint`. The brief only suggested a rename.
- `workflow_tool_pushed_accepted_delivers_after_run_terminal` stays. It starts the receiver after the
  run, so it now covers delivery retries surviving the run; the new test covers delivery during it.
- Not ported: `repro_refute_hostdrop_v5e.rs` tests asserting the old loss (replaced by the order tests),
  `repro_selectarm_flip_v3j.rs` (models the pick, and clippy now guards it), and
  `repro_rollovergate_noop_refute_e7w2.rs` (replaced by the live no-op rollover check), and
  `repro_refute_rollover_spin_p4n.rs` with `repro_rolloverspin_refute_y8n.rs` (replaced by the state-level
  blocker tests). Nothing is ported from `repro_wfspine_core.rs`: its H-14 test shows the harness plans a
  generation after a start batch, which stays true, and its other tests belong to later packages.

Verified: `cargo test -p temporal-workflow --lib` (174 passed), `cargo test -p temporal-workflow --test '*'`
(3 passed), `cargo test -p harness` (309 passed), `cargo clippy -p temporal-workflow --all-targets` and
`-p bots` with `-D warnings`, and `cargo check --workspace --all-targets --locked`. The workflow contract
exporter leaves no diff. No live test ran: the Docker daemon was down and starting it is out of bounds.

Written but never run, for the checkpoint:

- `workflow_tool_start_goes_out_before_the_next_generation_finishes`
- `workflow_tool_pushed_accepted_delivers_before_the_next_generation_finishes`
- `workflow_tool_auto_cancel_cancels_started_execution`, which now uses `CALL_NOWAIT` in place of the
  short await
- the no-op rollover check at the end of the continue-as-new test in `sessions_live`

Open:

- For `tool-dispatch-per-call`: the commit boundary runs after a whole batch. `execute_call_group` commits
  per call with no boundary between them, so a workflow-tool call moved onto that path holds its start
  until the batch ends. Check this when moving them.
- For `state-pruning`: `has_due_start` and `has_due_promise_deadline` now scan `start_requests` and the
  pending promises at every commit, which is cheap only once those maps are bounded. Prune at a later
  boundary than the promise's terminal event, as the per-entry lookup relies on the record for that entry.
- For the docs package: `agent-loop-and-durability.md` lines 181-182 overstate pushed promise facts, since
  a pushed resolution can wait out one activity. The race wake predicates are unchanged by design.
- Not fixed, bounded: a due deadline held behind a compaction queues another `ResolvePromise` each pass,
  and the extras are no-ops once the first lands.

Review of this package:

- `cb81ac95` closes a hole in H-02. A blocked rollover now keeps driving when the outer loop cannot clear
  the blocker, but a watchdog left by a finished cancel and a start backoff whose promise had ended were
  only cleared on an outer pass, and neither makes the drive yield. A queued run after a cancel, or a long
  run with no transport work, could then pass the threshold for its whole length and reach the server's
  history limit. Both are now cleared at each commit, with a state-level test. A cancelling run's watchdog
  also arms at the commit rather than the next pass, which only blocks rollover for that cancel.
- Checked and found sound: no `select!` is left and the arm orders match the package; every arm commits its
  own result and no other resume reaches the loop top; every `outer_loop_can_clear_now` term is consumed by
  a pass, so a yield from the top of the drive cannot repeat; promise status only changes in `append_events`,
  so the per-entry cancel queue misses no terminal; the bottom-of-pass check rolls over after a no-op
  operation; the workflow contract exporter leaves no diff.
- Left alone, already there before this package: a cancel landing while a call's result is being appended
  can let the next pending call in a batch be scheduled before the drain abandons it, and code tool requests
  still wait for the outer loop while a run keeps generating, since `transport_work_due` does not list them.
- Still unverified: Docker was down again, so nothing here has run against Temporal. The risky note above
  stands, and the same-activation order is still shown only by harness tests.

### `tool-dispatch-per-call` (H-03, H-70, H-05, H-33, H-45, S-08)

Landed:

- `0def183c` (H-03): `execute_call_group` keeps `NeedsApproval` outcomes and parks once when its window
  drains. `control::tool_batch_abandoned` is checked at the top of the loop, which covers top-ups, new
  readiness waits, re-dispatches and the park, and it replaces the Active check in the readiness races.
- `73866431` (H-70): `execution_groups` skips `await` and workflow-tool calls, and a dispatch with no
  groups goes to `tool_invoke_batch`. The activity's MCP merge and approvals branch,
  `ToolBatchOutcome::AwaitingApproval`, and the inline route, domain setup, denial, blocker and
  selection paths of `SessionTools::invoke_batch` are deleted. S-05 falls out of this.
- `f239250b` (H-05): the one numbering rule, `batch.promise_id_base.max(last_promise_id + 1)`, in
  `next_tool_batch_request`, with the slot comments corrected and tests with minted siblings.
- `2fe5c3ac` (H-33): `read_tool_args` returns a parse failure as that call's failed result and keeps a
  blob-read failure as an error, through one helper on every route. An erring await keeps its siblings.
- `14f621b1` (H-45): injected MCP specs register as RemoteInteractive, and the overrides are gone.
- `058b6ac1` (S-08): the saturating add and the corrected comments, with the regenerated workflow
  schema, because the `PromiseId` doc comment is in it.
- `98c101f6`: the mixed-batch live scenario now gates two calls in one parallel group.
- `aaa72eb3` (H-33): the malformed await test uses a real `agent_spawn` as its sibling.

Calls made:

- A call's window never holds a park. Ungated siblings finish first, so the Parked case in
  `tool_batch_abandoned` is only a guard, and the top-up loop needs no Active check. The refusal of a
  second park comes from `dry_run_append`, so `native_mcp_approval_proposals` gets no status check of its
  own. A test pins that it is refused before reaching the log.
- `SessionTools::invoke_batch` refuses an ordinary call with a failed result instead of keeping an inline
  route for it. Keeping it would leave a second promise numbering beside the per-call slots. Its tests
  moved to the per-call path through a test-only `invoke_each`.
- Groups form over the per-call calls only, so `[read, spawn, read]` is one parallel group. The spawn has
  no effect until commit, so nothing it does can be overtaken.
- `invoke_mixed_await_batch` and `invoke_lone_await_batch` merged into `invoke_batch`, because the
  impossible `Deferred` and approval arms were most of them. The package's "keep `completed_results`"
  applies to the merged code.
- The worker takes a native MCP call's deadline from `request.execution.class`. The H-45 judge left the
  literals because the batch request carried no class, and the batch path no longer runs MCP calls.
- The unit activity's race uses `tool_batch_abandoned` too, leaving `tool_batch_still_wanted` only for
  the check between groups.
- `[environment.activate, job_submit]` now pins the job to the newly activated environment, because the
  activation commits first. It used to pin the old one from the same request. I kept this as better.
- S-10 stays undone: the automatic re-dispatch is gone and the residue never contains `run_process`.

Deviations from the briefs:

- Not added: the H-03 live scenario with a slow ungated sibling and a steer during the park. A park can no
  longer overlap a call, so it would only cover a steer during an ordinary window. The predicate has a
  unit test, and that loop has no test without Temporal.
- Not ported: the `repro_refute_admissionpark_h7y`, `repro_wholesystem_parkabandon_g5d`,
  `repro_cancelconsumed_refute_w8d` and `repro_refute_readinesscancel_n6x` tests, which run a synchronous
  copy of the loop. Their harness facts are covered by the new drive tests and their predicate by
  `control::tests`. The two-park, slot-collision, `[agent_run, gated]` and `[sleep, agent_spawn]` shapes
  are ported to `drive.rs`, and `[agent_spawn, await{timeout:5}]` and the blob-read case to
  `session_tools.rs`.

Verified: `cargo test -p temporal-workflow --lib` (179 passed), `cargo test -p temporal-workflow --test '*'`
(3 passed), `cargo test -p harness` (316 passed), `cargo test -p temporal-runtime --lib` (476 passed, 1
ignored), and `cargo check --workspace --all-targets --locked`. The workflow contract exporter leaves no
diff. No live test ran: the Docker daemon was down and starting it is out of bounds.

Written but never run, for the checkpoint:

- `temporal_live_native_mcp_mixed_await_batch_parks_once_and_completes`, now with two gated calls.

Open:

- The note left by `host-loop-boundaries` does not bite: workflow-tool calls stay on the unit activity, whose
  single commit reaches `drive_boundary_after_commit`, so no start is held behind per-call commits.
- `execute_call_group` still ignores what `drain_pending_admissions` returns, so a drain that admits nothing
  would spin through the loop. The wait matches what the drain admits, and this was already there.
- For `rename-vocabulary`: `NativeMcp` and `Native` stay until then. The error text "requires batch-unit
  execution" stays too, because `mcp_live` asserts on it.
- For the docs package: `how-it-works/tools-and-controller-workflows.md` lines 42-60 still describe batches
  with `await` or a workflow tool as one activity, and scope the parallelism rule to "ordinary" batches. A
  page on approvals should say that a spawn beside a gated call waits for the decision.
- A failing assertion in the `temporal-runtime` lib tests aborted the whole binary here with `failed to
  initiate panic, error 5` instead of reporting it. It only showed on a real failure, and I did not chase it.

Review of this package:

- `cb13d0b2`: the admissions arm of `execute_call_group` returned a drain refusal with `?` and left its siblings
  running, while the commit arm cancelled them. Both now share one abandon path. It also removes a tool
  batches test that set a spec and read the same spec back.
- `dcc6cacf`: adds a test that a batch can park again on a later dispatch, the `[gated, exclusive, gated]` shape,
  with replay. It passed without a code change. It also corrects two comments that still said calls run
  through a park, and moves the cursor test out from between another test and its doc comment.
- Checked and found sound: every item is done as the package states. A preempted dispatch always means the
  batch is abandoned, so no dispatch can return without progress and spin. The top-of-loop check runs
  before the break that parks. Sibling summaries come from the whole dispatch. Every production registration
  of a native MCP tool declares its class, including `mcp.find_tools` and `mcp.call`. Code tool reservations
  run only on outer loop passes, when no dispatch is in flight, so they cannot take a slot from one. The
  live mixed-batch scenario reads correctly against the new flow. The workflow contract exporter leaves no
  diff.
- One behaviour change the package notes did not name: `mcp.find_tools` now gets its declared retry-safe
  three attempts, where the removed override forced one. It is read-only, so I kept it.
- Left alone: `call_request` and `PromiseIdAllocator` still add without saturating, which the S-08 scope
  leaves out and no production path can reach. A selection behind a gated call can still succeed after an
  earlier selection in the same batch failed, as in `[activate A, gated, activate B]`, because the later
  dispatch no longer lists A as a sibling. Only one selection takes effect, so it is harmless.
- Still unverified: Docker was down again, so `execute_call_group` and the whole two-activity flow have
  never run against Temporal. The risky note above stands.

Verified: `cargo test -p harness --lib` (317 passed), `cargo test -p temporal-workflow --lib` (178 passed)
and `--test '*'` (3 passed), `cargo test -p temporal-runtime --lib` (476 passed, 1 ignored), and
`cargo check --workspace --all-targets --locked`.

### `promises-and-awaits` (H-10, H-12, H-29, H-30, H-47, H-48)

What landed:

- `9957b035`, `17a61371` (H-12): `NativeMcp` is now `{ batch_id, call_id }` and
  `remote_mcp_call_runtime` matches on both, so a later batch's reused call id asks again.
  The batch id is stable across the pending-only re-dispatch. `OpenAiMcp` keeps its shape.
  The second commit fixes an `api-projection` test the first one stopped compiling.
- `7c804b05` (H-10): all three invalid-await paths pass the session id and the session-less
  helper is gone, so a bad await stays a failed call instead of failing the drive. New
  well-known blobs cover await rejected, deadline exceeded, receiver cancelled and empty
  success, and the cancel text names no cause. The code and model resume paths share
  `joined_call_outcome`.
- `de81385a` (H-47): the gate refuses a source resolution naming a timer. A delivery for an
  absent promise is a no-op recorded on `last_warning`, and one for a terminal promise is
  silent. `validate_policy` refuses the reserved kind and a zero promise deadline at binding
  admission and in the worker, never in a validator the reducer calls.
- `deabded1` (H-48): the existing best-effort notice goes out when a promise fails with the
  deadline error ref, derived from the applied event with no in-memory set.
- `23cd0598` (H-30): poll results join `pending_source_resolutions` with the promise's own
  producer, so the schema check applies. Query errors stay typed and retryable ones return
  Pending. A sweep shares one Pending answer per execution, and polls back off from 10 s to
  a 60 s cap.
- `cad43559` (H-29): a detached follow-up calls `materialize_await_result` for its one
  promise. The run input is the summary, the result ref, then the additional context.

Calls made:

- Two well-known blobs beyond the planned four. A batch conflict, because the await-rejected
  text is false for a join beside an await. Failed without detail, because a bot lane and
  channels do report `Failed` with no error ref.
- H-48 went ahead although the review judge would have left it, because the package
  instructions ask for it. The notice and contract wording now say the session stopped
  waiting, which covers both causes. The workflow contract was regenerated with it.
- The poll has its own bounded `recovery_poll_activity_options`, and an exhausted poll logs a
  warning and asks again later rather than failing the session.
- The recovery query has a 20 s RPC timeout, because the poll blocks the outer loop and a hung
  query should read as a failed one.
- `validate_workflow_tool_reply` returns `Invalid` when the payload blob is missing, because
  routing polls through it widens exposure. The wider ingest gate stays with `store-rejections`.
- A terminal promise's delivery is dropped with no warning at all. The instruction allowed
  `last_warning` at most, and conversation lanes answer late routinely, so a warning would be
  noise. The absent-promise case does warn, and the test asserts `last_warning`.

Deviations from the briefs:

- H-29's joined variant is not here: a `Joined` binding with no reply schema and a non-UTF-8
  `Resolved` payload still resolves, and the raw bytes reach `read_text` as the tool result.
  The brief routes it through the ingest gate, which `store-rejections` adds. That package
  must return `Invalid` for this case and fail the promise, so one binary reply does not break
  the next generation.
- H-29's brief also lists `attachments` from the materialized result. A run input has no field
  for them, so they are dropped. See Risky calls.
- Ported as tests beside the code: `repro_refute_mcpidreuse_k8t`,
  `repro_refute_awaitsibling_r2d`, `repro_refute_unavailabletext_w5k`,
  `repro_joinedresumedrift_refute_m2q`, `repro_refute_pollschema_h4w`,
  `repro_pollquery_race_n6f2`, `repro_execkind_managed_path_q7k2`,
  `repro_timerforge_refute_s6b` and `repro_refute_timerresolve_m2z`, in the behaviour they
  argued for. Each covers that repro's main case, not all of its variants. The signal-before-poll
  order from `repro_pollquery_race_n6f2` has no direct test, because `process_due` needs Temporal;
  only the final-or-Pending classification and the gate are unit tested.
- Not ported: `repro_refute_detachedfollowup_raw_a8s`. It pins the raw entry staying in context
  after a failed generation, and follow-ups no longer create that entry. Its intent is covered by
  `a_single_promise_result_carries_its_image_and_keeps_binary_bytes_out` and the input shape
  assertions in `detached_session_promise_resolution_produces_followup_candidate`.

Verified: `cargo test -p harness` (323 passed), `cargo test -p temporal-workflow --lib` (183
passed), `cargo test -p temporal-runtime --lib` (479 passed, 1 ignored), `cargo test -p
api-projection --lib` (45 passed), `cargo check -p temporal-workflow -p temporal-runtime
--tests`, and `cargo check --workspace --all-targets --locked`. The workflow contract exporter
leaves no diff. Clippy and the full workspace suite were not run, as they belong to checkpoints.

Written but never run, for the checkpoint:

- `workflow_tool_start_recovery_query_meets_the_reply_schema`. The Docker daemon is down and
  starting it is out of bounds, so no live test ran in this package.
- No test covers `queue_detached_promise_followups` calling the activity against Temporal. The
  unit tests cover the candidate, the input assembly and the activity separately.

Open:

- For `store-rejections`: the joined binary payload above, and materialization failures of a
  follow-up, which the ingest gate should reduce to store outages.
- For `state-pruning`: `OpenAiMcp` approval records carry no batch id, so they need their own
  handling when pruned. The await-rejected text already says an id may be no longer retained.

Independent review of this package:

- It starts at `af6f8b4e`, so `9957b035` is its first commit.
- The risky note on a detached follow-up was wrong and is corrected above. It said an unreadable
  payload fails the workflow after three attempts, but `activity_options()` has no retry bound,
  so the outer loop stalls instead, and it blamed a missing blob, which stalls earlier at append.
- A poll's next check was measured from the start of its sweep. After a slow sweep the backoff
  was already due, so checks ran back to back. It now measures from the current workflow time
  (`87a36549`).
- Checked with no change: replay safety of `process_due` and the follow-up activity, the rollover
  block on `pending_source_resolutions`, `validate_policy` staying out of what the reducer runs,
  no first-party binding claiming the reserved kind, receivers keying the deadline notice by
  invocation, and a media entry passing run input validation.
- Still unverified: the query status mapping and the live test, because the Docker daemon is down.
