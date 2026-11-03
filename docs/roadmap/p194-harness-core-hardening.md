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
- **Run admission now checks the session's own tool choice.** A session whose choice
  dangles refuses every new run at the door, with a clear error, until the config or
  toolset changes. Before, it failed at the first turn. A queued run admitted against
  an older toolset still reaches FailRun.

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
the two apart today.

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
