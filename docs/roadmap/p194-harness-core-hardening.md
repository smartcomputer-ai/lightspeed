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
  events a day. A lane parked on its run no longer blocks continue-as-new (see the
  entry below). With the 24 h timer gone, a session stuck Running holds its lane
  until an operator force-closes it.
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
- **Unreviewed documentation.** Rust reference text ships with its code, and all guide
  and upgrade wording sits in one code-free commit, `54ae3cfd`, for review afterwards, as the
  owner allowed. The root README and `AGENTS.md` are not edited; wanted wording is under
  `docs-and-review-notes` below.
- **The sub-agent default deadline.** `SUBAGENT_DEFAULT_DEADLINE_MS` (1 hour, 24 h
  ceiling) is a default limit against the north star, and no decision covered it.
  Behaviour is left unchanged; this needs an owner decision. Resolved 2026-11-04: `run_timeout_ms` is optional with no default or ceiling, `agent_run` and `agent_spawn` carry no hard promise deadline, and a 15 minute liveness check fails the delegation when the child's session workflow stops.
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
- **Run admission checked the session's own tool choice until `config-admission`.** That
  package moved it back to FailRun, so this no longer applies; see its entry below.

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
- **`store-rejections` has never run against Temporal or Postgres.** Docker was down. Nothing has
  shown Temporal stopping on these non-retryable failures, a data rejection reaching FailRun, or
  control admissions landing during scope preparation. 22P05 and 22021 come from the review's run
  against Postgres, and the mapping is unit tested with a fake database error. Run
  `pg_live_a_value_postgres_cannot_hold_is_a_data_rejection_and_writes_nothing`,
  `workflow_tool_reply_schema_gates_resolutions`, `code_workflow_live` and `sessions_live` at the next
  checkpoint.
- **A store-rejected admission fails only that command, not the run.** The instruction said to admit
  FailRun for a rejection while a run is active. I applied that to the drive path only. A rejected steer
  or approval note gets `rejected` with kind Internal, because its caller can be told and the run is not
  at fault. A rejected `ResolvePromise` has no caller, so its promise stays pending until a deadline or
  a cancel. The ingest gate makes that unreachable unless the store misbehaves.
- **A head conflict and a missing session now end the workflow.** They used to retry for ever, so the
  session looked Running. A race with a direct writer now gives a visible failed workflow, which only
  the session restart package can bring back. The review traced that race as rare, not as impossible.
- **A log that will not reduce fails bootstrap at once.** This covers a gap in the log and an event that
  does not decode or apply. A row whose envelope is not a stored entry still comes back from the store
  as a plain failure and retries without limit. I left that, because the envelope has not changed and
  telling it apart needs another store variant.
- **The ingest gate adds an activity to most replies.** Any resolution carrying a ref now costs one
  activity on the outer loop and a `retain_blob` write, and a joined reply reads its whole blob to check
  UTF-8. A reply with no ref and no schema costs nothing. A fan-out of many replies waits behind them
  one at a time, and a store outage blocks there, as it does at the append.
- **One attempt for code tool scope preparation.** A failed inventory fetch now fails the script's scope
  with no retry, where an outage used to wait for the server. This relies on `mcp-native-default` for
  the negative entry that makes a second try pointless. Until then a blip fails that one script. The
  script gets a readable error and the run continues. Closed by `mcp-native-default` for one failing
  server: `392b857b` makes a failed fetch return the last list or none, and the 38 s limit outlasts the
  30 s fetch. Fetches in one preparation run one after another, so two or more servers hanging at once
  can still outlast it and fail that preparation. The owner accepted this.
- **A model answer that is exactly a digest fails the run until `typed-blob-refs`.** Its preview looks
  like a blob ref, so the store reports `MissingBlobs` and the run fails with that message. Before this
  package it stalled the session. The next package stops previews counting as refs. Closed by
  `typed-blob-refs`.
- **A malformed emission is dropped, not answered.** A plugin signal with a bad id or blob ref cannot
  be attributed, so it logs and drops. Its promise stays pending until its deadline, if it has one, and
  the plugin hears nothing. The session's `last_warning` shows it, but the other five receivers keep
  only an error log. The gate used to fail the promise for a malformed ref, because that ref decoded.
- **A ref in a plain string now retains nothing.** Roots come only from `BlobRef` values. The
  cross-check that catches a writer doing otherwise runs only in the in-memory store, so it sees only the
  writers tests exercise. A ref it misses gets swept after the grace period and fails a later read.
  `AttachmentSource.id` is deliberately in that group: a `vfs_snapshot` source's snapshot can now be
  collected while the attachment lives. Nothing resolves that id, but the web shows it.
- **Append decodes every event.** One decode and walk per event, inside the Postgres transaction. The
  earlier prototype measured about three times the shape scan: 1.5 to 2.3 ms for 2,000 context entries.
  An event that does not decode now fails its run as `DataRejected`, instead of committing and failing
  every later read of the log.
- **`typed-blob-refs` breaks stored data.** Logs written before it hold workflow emit effects with their
  refs in `data`, which `invocation_from_emit_effect` now refuses, and VFS commit effects whose snapshot
  ref roots nothing. Purge dev state at this release, as the branch already requires.
- **Older workspace heads stay rooted for the life of the session.** Typing the VFS commit effect's ref
  keeps today's retention. A session with thousands of VFS edits holds every head until it is deleted.
  Pruning them was left out of scope.
- **`typed-blob-refs` has never run against Temporal or Postgres.** Docker was down. The live test that
  sends garbage signals at a parked run has not run, so the claim that `RawValue` survives them rests on
  reading the SDK and on unit tests of the decode. The Postgres live fixtures were rewritten to real
  events and only compile. Run `workflow_tool_reply_requires_exact_stored_producer`,
  `store_pg_live`, `session_lifecycle_live` and `audience_live` at the next checkpoint.
- **What a turn limit counts and when it fires.** Context-recovery and steering
  turns count, because the limit is "turns started". A final answer that still has
  unheard steering fails as `limit_exceeded` instead of completing, so a client does
  not read the steering as handled; the answer stays in context and the steering
  reaches the next run. An overflow on the last allowed turn fails at the limit without
  compacting. A pending approval on the last allowed turn still parks and asks, then the
  run fails once it is decided, so an approved hosted call only proceeds in a later
  run's request. An operator who sets a low `maxTurns` on a session with recovery
  compaction can lose a run to a limit they thought was about model steps.
- **Dropped failure kinds and `maxToolRounds` have no compat decoding.** A stored event
  holding `tool_failure` or `cancelled` as a failure kind no longer decodes. A stored
  profile or client sending `limits.maxToolRounds` is refused, because the API's
  `LimitsConfig` denies unknown fields, while the run-level `RunLimitsConfig` ignores it.
  This fits a breaking release with purged dev state.
- **A pruned approval, promise or invocation answers differently than it did.** A
  second decision on a decided approval gets `not_found` once the record is pruned,
  instead of "already decided". `await` on a pruned promise gets the await-rejected
  text. A retried submission older than the last 256 terminal runs starts a duplicate
  run. `FailWorkflowToolDelivery` and `FailWorkflowToolStart` on an unknown invocation
  id are now no-ops for any id, since invocation ids carry no cursor to tell "pruned"
  from "never existed". A typo there is logged and otherwise silent: at info for a start,
  and as `last_warning` for a delivery, since a notify receiver that was given up on
  never heard.
- **Closing an absent scope answers closed with no calls, for any id.** A late close had
  to be harmless. The cost is that closing an id that never existed looks the same. The
  query still rejects an absent scope as `UnknownScope`, because the runner's lost-receipt
  path depends on it. A runner finalizing after its scope was pruned therefore tries to
  open it first, is refused because the parent is gone, and then closes it, which clears
  the error. The workflow contract text says this.
- **An update waiter whose scope left state gets `UnknownScope`, not an outcome.** It
  needs a run terminal to prune the scope in the same step while a call is still in
  flight, which takes a parent whose promise was already terminal. The usual force
  cancel keeps the interrupted outcome readable, because the parent's promise is only
  cancelled after the terminal event, so the scope lasts until the next boundary.
- **The code call budget check is gone, as the owner's answer says, so the second
  concurrent script call at the limit fails late.** A script can admit more calls of a
  full tool than the limit, and each is refused when its result is committed, as a
  failed call with the shared message. `process_pending` already turns that rejection
  into a failed call and not a workflow failure. A model batch cannot reach this
  rejection in normal use, because the worker counts its siblings first.
- **Old run summaries have no usage, and old run details leave out compaction usage.**
  Usage is accumulated in state and never written on a run event. A summary from the
  log omits it, as its API doc now says. `session/runs/read` totals the run's generations
  from the log, but compaction events carry no run join, so their usage is not in that
  total. The UI will show a blank for usage on runs older than 256. Adding usage to the
  terminal event would fix both, if that is wanted.
- **Old run reads scan the session's log.** `read_run_record_entries` filters
  `session_events` by session and kind with no index of its own, as agreed. Paging
  through runs older than 256 in a session with millions of events will be slow, though
  it is rare and nothing on the session's own path calls it.
- **State has settled on a higher floor than the card said for code mode.** One
  `code_execute` per turn stays flat through turn 5,000, at about 178 KB and not near
  35 KB. Nearly all of it is the 256 terminal promises at about 520 B each and their
  settling order. That is 12%
  of the budget and does not grow. Lowering the window would shrink it, at the cost of
  an `await` on an older id being refused sooner.
- **A very long run still cannot be read whole through `session/runs/read`.** It refuses
  a run over 20,000 events and points at `session/events/read`, which at about 8 events a
  turn is near turn 2,500. This was already so and is now reachable. Left alone,
  because the refusal is typed and the events API serves the run.

- **A failed body read is now retried on every client.** A connection reset, truncated body or deadline
  after the headers is a resend within the LLM retry budget. The provider may already have served that
  request, so a retry can bill a second generation, and a provider-hosted MCP call in it can run twice. That
  matches the at-least-once already accepted for provider-hosted calls. Production generation is all
  non-streaming, so this covers all generation traffic. Before, a reset failed the run on every client, and a
  deadline after the headers failed it on Anthropic and Completions. A connect timeout on any client now reads
  "request timed out after" the request deadline rather than the connect timeout, as it already did on Responses.
- **The compaction estimate no longer refuses anything by itself.** A chunk with no smaller cut goes to the
  provider, however far over the limit it looks. That relies on the provider answering with a typed overflow.
  A compatible backend that truncates silently, or that sends an overflow the client does not classify, gets
  an oversized summary request or a plain rejection instead. The bytes/4 and bytes/8 hints undercount
  dense text and small PDFs, which costs refused calls from the 32-call budget and not a failure. They also
  overcount scanned PDFs, and a retained tail is part of every chunk's size. A 2 MB PDF in the tail on a 200k
  window makes every chunk the smallest cut, so a prefix with more than 32 cuts fails on the call budget until
  that PDF ages into the prefix. The input budget is checked after a call, so it can overshoot by one accepted
  chunk. "Did not reduce its input" still compares estimates, which a summary larger than a PDF-only request can
  trip when that request is just over its 4,096-token floor.
- **Only Anthropic's overflow wording was added.** As instructed, the shared hints are unchanged, so a backend
  with its own wording, such as Gemini's OpenAI layer ("input token count ... exceeds the maximum"), still fails
  as `request_rejected` and repeats until someone compacts. Of the two Anthropic phrases, only "prompt is too
  long" was confirmed against the real API. "exceed context limit" is recalled wording for models before 4.5,
  and classifying it as overflow is harmless if it no longer occurs.
- **A bad tool name or call id still ends the run.** This is the owner's choice, reported properly now. Known
  triggers (gpt-oss names on vLLM, an empty Gemini id, LiteLLM's signature in call ids) stay unusable, the valid
  siblings and text of that turn are dropped, and a bot run ends with no chance to recover. Chat Completions
  still fails on a missing id where the other two fall back to an index, because evening that out is the rejected
  option's fallback.

- **A user's next message can wait behind a compaction.** Proactive compaction moved from run end to the next
  run's start, so the wait that a parent or a bot lane used to pay now lands on whoever starts the next run, up to
  the LLM activity budget. A session the user never returns to pays nothing. Compaction also no longer tidies an idle
  session, so a read of an idle session can show context over the watermark.
- **The failure floor holds off proactive compaction near the limit.** After a failure the token trigger waits for
  10% growth, which on a 200k window with a floor near 190k means the provider's overflow arrives first, and overflow
  recovery does the work. A temporary outage costs a 10% wait after it heals, and any success clears the floor. Nothing
  else clears it: lowering the threshold, removing entries or switching to a smaller model leaves the old floor in
  place until a compaction succeeds. Overflow recovery is the backstop there too.
- **The 1,500-entry trigger compacts sessions that never asked.** A low-token session on a large window loses verbatim
  history and its prefix cache at 1,500 entries, and pays for a summary it did not need for tokens. It applies to
  ProviderTriggered sessions too, which run standalone compaction here for the first time unless a provider summary
  already existed. Whether every provider behind that policy gives a good standalone summary has not been tried.
- **The guard's failure message does not carry compaction's own error.** It says to enable or run compaction, as
  specified. The real reason is on the `ContextCompactionFinished` event and in the transcript, one event earlier.
  Someone reading only the failed run sees advice that does not fit a provider outage.
- **A run stopped at the guard before its first turn loses its input from context.** It is failed before the input is
  written, so the session stops growing, which is the point of the guard. The input stays in the log on `runAccepted`,
  but the next run's model does not see it and the web transcript shows no user message for it after a reload. A bot
  event in such a run ends as `run_failed`, and the bot does not see it when the session heals. Writing the input first
  would add entries per failed run on a session that is already at the limit.
- **Compaction at the guard costs the last two turns.** At 2,000 entries or more a compaction covers the whole settled
  window, not just the older part, because otherwise a large tail fails every run for good. Reverting the guard means
  reverting `e05445d5` first.
- **These compaction changes have not run against Temporal or a real provider.** They are unit and replay tested only.
  `sessions_live` and both provider `*_compaction_live` suites relied on idle compaction and were rewritten to start a
  run, and only compile. Run them at the next checkpoint.

- **Capacity costs Anthropic runs a model listing.** Each run on the `anthropic` route with no declared limit now reads the
  discovery cache during preparation. The cache keeps a success for 10 s, so a quiet session pays a `GET /v1/models` per run
  start, and in an outage a run can wait up to the 15 s discovery timeout before admission. Other routes return at once.
- **An idle session that has not run shows no reported capacity.** The settings view reads `contextLengthError` as its
  threshold source on a fresh Anthropic session until the first generation records a limit. A session whose model was
  changed while idle shows the last generation's limit until the next run. Both are display only: a run reads its own value.
- **Only one live suite sees reported capacity.** `ActivityState` gets discovery only through `UniverseRuntime`, so live
  suites that build their own state resolve nothing and exercise the fallback. `run_capacity_live` is the one test that
  proves the wire, and it covers a single Anthropic run.
- **Model switches are only proven where they were tested.** Removing the pin relies on the OpenAI and Anthropic requests in
  the owner's answer. Chat Completions routes and Anthropic's native compaction block across models were not tried. A 200
  does not show the new model used the old reasoning. A refusal fails each run with the provider's text until someone
  switches back, and nothing warns before it. Idle manual compaction still uses the last generation's model, so after a
  switch away from a retired model it can fail while a run on the new model works.

- **What the model reads is no longer always the log's order.** A note appended during a batch, or media from one of several
  parallel calls, is stored between a call and its result but renders behind the group's last result. The transcript and the
  public `active_context` view keep arrival order, so in that case they differ from the request. A note can also render ahead
  of a media entry from an earlier result. The media entry labels itself with its `media:` handle, but no test shows a model
  still pairs it with the right result. The fix rests on OpenAI's documented rejection, not a live one, and Responses was never checked with the old order.

- **Waiters now answer by token, which leaves a few honest gaps.** A manual compaction waiter accepts any manual outcome newer
  than its baseline, so another caller's finished compaction can answer it while its own command is still queued; that command
  then runs or is refused with nobody listening. A decision whose run has ended reports `decided` when no record is left to
  compare, unless its own refusal was seen first, so a decision that lost to a cancel in that window reads as decided. The list
  of 256 failures could drop a refusal only if 256 newer ones land between two polls of one waiter.
- **`operation_pending` is wider than the brief's examples.** Every expired wait uses it, including session open, session
  preparation and environment job start. It is retryable for bots and sub-agents, and bot tools treat it as infrastructure as
  they did the old internal timeout. A retried non-force close that is held behind a compaction queues a second close, and the
  second is refused harmlessly after the first lands. The backend maps it to HTTP 409 with `kind`, since there is no better
  status in `withGateway`.
- **The web retries a held start or steering for as long as someone watches.** `postUntilAccepted` has no attempt limit, because
  compaction has its own budget and each attempt waits about 90 s on the server. It stops when the session view changes; the
  request still lands.
- **A bot steering can still be delivered twice in one narrow case.** An attempt times out with its signal queued, the batch
  lands and the run ends before the retry starts. The retry reads no running run and falls through to an ordinary run. The dedupe
  set is the active run's retained batches, 64 of them, so it cannot see this. The waiter's log check only covers a run that ends
  during that attempt. Bots are work in progress, so this stayed.
- **A long run read shows less than it holds, and costs the whole interval.** Past 20,000 of its own events `session/runs/read`
  keeps the opening and the latest whole turns. A single turn past the limit keeps only the opening, since a partial turn would
  project inconsistently. Every read of such a run still pages and decodes its full interval. The response stays exempt from the
  budget, so 20,000 events can still make a very large document.
- **A session's dangling tool choice no longer refuses at the door.** This reverses the `core-failrun` call. A choice left naming a
  left-out MCP tool now admits the run and fails it with `tool_choice references missing active tool <name>`, and every later run does
  the same until the document changes. A wake-up run spends itself this way rather than being lost. `ReplaceSessionConfig` lost its
  check too, so a profile naming that tool is not refused. Preparation still refuses a dangling choice when nothing was left out, but
  while a server is left out a typo in a put gets through as well, and it fails at the first run instead.
- **A left-out MCP tool is silent to everyone but the model and the log.** The run proceeds with less than its document asks for. The
  notice is a catalog entry the model reads and an operator can see in session context, plus a warning log per run. No API field or
  UI badge says a session is degraded, and an automation that needs the tool only learns from the model's answer.
- **An operator's narrowing also degrades.** Narrowing `allowed_tools` below a session's subset, or a grant going inactive, leaves the
  whole server out, not just the missing tools. This follows the rule "an attachment that cannot resolve", and keeps the failure visible
  rather than quietly offering less than the subset.
- **History after a provider-executed MCP tool is left out is untested.** A context holding that server's `mcp_tool_use` blocks now goes
  out with the server undeclared. Attachment removal already did this, but degrade makes it routine. If a provider refuses it, the run
  fails with the provider's message and keeps failing until the context is compacted or the server returns. No live provider test
  covers it.
- **A closed default on a profile path still refuses where it did before.** The brief expected it to fail at tool use, but `prepare_profile`
  checks the fill candidate's selectable state, so a profile with a closed default fails when the pointer would be filled: a start
  (closed on a server-allocated id, retried on a client one) or an apply, which a bot ensure surfaces as an activity error. I added no
  check and removed none. A bot on such a profile stays stuck at ensure, and `bot-delivery-wedges` is the place to decide.
- **A failed force close leaves the old leak.** If stamping or closing fails after a setup failure, the caller still gets the setup error
  and the session stays live and possibly unstamped, with only a warning log. Accepted because that needs an outage inside a failure path.
- **Some catalog edits still refuse runs at preparation.** Degrade covers an attachment that cannot resolve, and nothing else. Renaming a server's label so a session's
  tool choice dangles with nothing left out, flipping its execution to provider-executed under a Completions session, or a label collision all still refuse each run at
  preparation, and a wake-up run that hits this is lost as before. These are rare edits, and the caller hears the real reason, so I left them.
- **A setup outlasting the 90 s wait is not settled.** `wait_for_open_session` stamps and closes only on a setup error it sees. If the wait expires first, the caller gets
  `operation_pending` with the id in its message and the session stays unstamped. If setup then fails on a server-allocated id, that session stays live, which is the state
  H-72 removes. A Platform non-admin cannot reach it. A key holder can list and close it.
- **A full MCP tool definition above 64 KiB gets cut.** The native budget covers every native outcome, so `mcp_find_tools` in names mode is cut too, and the
  model gets invalid JSON ending in `[truncated]`. Its own hint promises "the full definition", and five names can exceed the budget together. Asking for one at a
  time helps unless one schema alone is larger. Exempting this path would leave a 5 MiB ceiling from a third-party server, so I kept the single rule. An argument error
  quoting a large input schema is cut the same way.
- **Other tool results stay unprojected.** `await` results and joined workflow tool payloads still reach context uncut, as the H-19 judge noted. A sub-agent's output
  can reach 512 KiB, and one await can combine 32 promises, so the same oversized-entry failure stays possible there. `oversized-input-refusal` is the next backstop.
- **A store failure in the LLM activity can now bill a second generation.** Blob store failures, stored-credential lookups and MCP record reads are retried under the
  generation budget, so one that happens after the provider answered sends the request again. `BlobStoreError::Store` also covers "no object store is configured", so a
  blob over the inline limit on a PostgreSQL-only deployment now retries for up to 21 minutes before the run fails, where it used to fail at once. Telling them apart
  needs another store variant. This slice is its own commit, `346c029e`.
- **A provider-hosted server with no usable record is left out with only a warning log.** That covers a record deleted, disabled or moved to native during a run. The notice from
  `config-admission` appears at the next preparation, not at the turn that lost the tool. It adds to the untested case of history holding that server's `mcp_tool_use` blocks.
- **A running run can see a list from a moved server.** The inventory cache stays keyed on the spec's revision, and a refresh reads the live URL, so after an admin edit a
  running run gets the new server's tools under the old key at the next refresh. Those tools are callable, since a call reads the record too.
- **A put through the CLI with no `--execution` now moves a provider-hosted server to native.** The command sends the whole document, so omitted flags take their defaults, as
  they always did. Only the default changed.
- **A transport fact can still persist outside session state.** The server record holds the URL in plaintext and returns it, and the optional provider request debug dumps
  still contain it.
- **Every carry-less bot start rolls forward, not only at a release.** Seeding no longer checks the bot row's age, so a controller terminated by an operator in the same namespace
  also moves to a fresh main session and fresh per-key generations, and loses that conversation context. Before, it went back to generation 1, whatever generation it had reached. Per-key rows from before the
  start are not tracked by the new controller, so nothing closes them, not even a bot delete, which covers only main generations and sessions the new controller routed to.
- **A repeated per-event id after a breaking release reaches its old row.** Per-event ids are skipped when seeding, so an event redelivered with the same id resolves to generation 1 of a
  row with no workflow in the new namespace. Traced by reading: an open row answers `SessionDead` and a closed one fails the run start, so the delivery ends `run_failed` and nothing wedges. A changed tool
  declaration rotates that id to `-g2` and delivers the event again instead.
- **A carry over the payload limit stops a controller from starting.** A start sends every retained per-key base, one entry each, of about 50 to 110 bytes. At Temporal's default 2 MiB that is roughly 20,000 to 40,000 distinct
  keys. Past that, a controller that is not running refuses events until the rows go. A running one is not affected, though its own carry reaches the same limit if those keys have all closed once. Fixing it means bounding per-key generations, for example by a store lookup at resolve time.
- **The compatibility script checks less than a replay would.** It sees the workflow crate, its four workspace dependencies and `temporalio` packages. A third-party bump that changes
  workflow behaviour, such as serde, prints restart-in-place. The opposite error is common: any change to those crates, tests and generated files included, prints breaking.

- **The oversized-text refusal covers little, and can refuse near the limit.** It only applies where a limit is known: a limit the document declares for its own model, or first-party
  Anthropic. Elsewhere, and for PDFs, images, `session/context/append` and a later switch to a smaller model, an entry that can never fit still gets in and fails each run as before. Bytes / 4 is
  only an estimate: dense or non-Latin text can pass and still block the session, and text averaging over 4 bytes a token can be refused although it would have fit, with no way to override except
  raising the declared limit on a route that has one. A small declared limit now also refuses ordinary input, so a limit of 1,000 tokens refuses a bot's 4 KiB event text, which ends that delivery as `run_failed`.
- **Large text can wait on model discovery at the door.** Text of 16k tokens or more on a route with no declared limit reads the discovery cache during the API call. In an Anthropic listing outage that is up
  to the 15 s discovery timeout on a start or steer, and the limit then counts as unknown, so nothing is refused. Every start and steer also stats each text entry, which is one more store read per item.
- **"The largest entry" is a hint, not a diagnosis.** It is the largest by estimate in the refused chunk. A tool group too big as a whole has no single culprit, and replacing the entry named there may not be
  enough. An overflow the client does not classify still ends as `request_rejected` with the provider's text and no name. Assistant output and instructions cannot be replaced, so when one is largest the text
  says so and points at a new session, a case the owner's wording did not cover, and a new session will not help if the instructions are what is too large.
- **A bot's start retries end after about 20 minutes, and a held start can land after that.** `BOT_START_MAX_ATTEMPTS` is 10, with a backoff from 5 s to 5 minutes, so an outage
  longer than that ends the delivery `run_failed` with the canned reply. A start held behind a compaction counts each 90 s hold as an attempt, and the run may still be accepted after the
  delivery has failed. Nothing stops it, so the bot can answer an event the operator sees as failed. The bound is one constant if this is too short.
- **A delivery waiting on a busy session has no limit and holds no lane.** It goes back to the queue and looks again every 5 s for as long as the session stays busy, with no attempt count, so a session
  stuck running holds its deliveries and costs one status read per look. Rollover stays open meanwhile. The snapshot lists it as pending rather than active, so the bot can read `idle` with a pending count.
- **Rollover no longer reconciles before the first pass.** A new execution starts dirty, and the loop's own gated attempt replaces the separate call. On a rollover with a busy main session, that
  session is not ensured until it goes idle, and main deliveries wait as they did before. Setup status stays `ready` from the carry during that time, so a controller that restarted over a session
  that has since gone bad reads ready until its first attempt.
- **A degraded bot retries forever.** There is no attempt limit, so a profile that can never apply costs one ensure per 15 minutes per bot, for as long as the bot is open and even while it is
  disabled. It rotates main at most once per failure streak, which is what keeps retries from burning generations. A later mismatch in the same streak degrades without rotating until a config signal.
- **A late event for a closed per-event session fails.** Superseded by `bots-dead-session`, which opens a next generation; see its entry. A `bot.reply` receipt, or a redelivery past the 2,000-id dedupe tail, used to reach a fresh `-g2` session. It now resolves to the closed
  session and ends `run_failed`. A reused bot id whose new incarnation sees the same event id does the same, since the seeding skips those bases too.

- **Stop now kills a foreground command.** A call dropped before its handle came back sends SIGTERM to the group, then SIGKILL after the grace period. A foreground command someone wanted to survive
  Stop is lost, as the owner chose. A worker shutting down gracefully drops its calls too, so a rolling deploy can now stop foreground commands that used to run on, if the runtime lives long enough to send the
  terminate. The call still fails at the boundary with the note, which then overstates a little.
- **A lost start response still leaves a command with no handle.** If the connection fails while `process/start` is in flight, the command may have started, the call fails as a plain tool error, and no terminate can
  reach it on that connection. Neither the handle nor the "may still be running" note covers this, because the note is only on the boundary path and the brief scopes H-38 to reads after a start. The window is one round trip.
- **An interrupted wait can report Running for a command that has ended.** It covers a close that means the environment went away and a read failing while draining output after the exit. The note says "may", and a later
  `continue_process` reports the exit or an unknown handle. `continue_process` itself is unchanged: on a lost connection it fails and drops the pages it read, since the model already holds the handle.
- **Head updates retry eight times with no backoff.** Eight was picked without measuring. A workspace busy enough to lose eight in a row gets `Conflict`, whose text says the file changed since it was read, even for an unconditional
  write that read nothing. A capture that loses its one revision check also reads that text now, in place of the generic `vfs_workspace_revision_conflict` failure. A write puts its blob before the head update, so a conflict leaves an
  unreferenced blob for the collector.
- **A move between two attached workspaces is two commits.** The destination goes first, so a failure in between leaves both files. Only a move within one workspace is atomic.
- **Cancel or a deadline during a patch's commit still reports nothing.** The future drops, so no `PatchPartiallyApplied` is built, and on VFS the commits that landed leave no effect in the log. The window is now only the commit pass.
  A file changing after planning, or an unreadable destination such as a read-only attachment, still gives a partial apply, by design.
- **Refusing an add onto a directory is one more than the list.** The agreed planning refusals name a directory as a move destination, not as an add target. It cannot execute either, so I applied the same check rather than let it
  fail halfway through a commit. A symlink to a directory counts as a directory for both.
- **A restart fails the run even when its work might have finished cleanly.** A generation or call cut off by the stop may have completed and been paid for, and the
  run reads as failed with no way to tell. This is the agreed trade against repeating a paid call or a `git push`. A non-retry-safe pending code tool call fails even if it
  never started, because the log cannot say.
- **"Parked" is wider than `RunStatus::Parked`.** A run waiting on `await`, a joined workflow or a script has status Active with a parked batch, and it also keeps waiting, since the
  code tool rule only makes sense if a script's parent survives. A reply a receiver sent during the outage is lost, so that wait lasts until the promise's deadline or a cancel, and a
  `Promises` completion can have no deadline.
- **Only a window is re-sent, and what else died with the execution stays dead.** The last 8 ended runs notify again. Pushed invocations with a pending promise on the active run or the
  session go out again however old they are, naming the log's head as `log_seq`, because state does not keep the original. Receivers dedupe on the emission id, but the bot controller's
  dedupe tail is capped, so a very old invocation could run twice. Notify-only invocations are not re-sent. Queued promise cancellations and `pending_execution_cancels` are lost, so a
  receiver may not hear of a cancel and a started execution may run until it ends by itself. Admission failures, operation receipts and any toolset still pending for a queued run are
  lost too. Traced in review: a run queued behind an active one keeps its prepared tools only in workflow memory, so after a restart it starts on the last published toolset, not on what it was
  prepared with. That is the one place a run can use an observation preparation has replaced, and the worst case is an MCP catalog or skill list that changed in the meantime; config cannot change
  while a run is active. A detached promise's follow-up run also sits in memory between the promise ending and its admission, so a stop in that gap loses the follow-up, and a compaction in flight
  is issued again, since `FailRun` leaves it pending: one repeated paid call, which a poison compaction answers with the existing refusal path.
- **`close_on_terminal` does not survive a restart.** Only `start_managed_session_for_workflow_with_profile` can set it, and only live tests in this repo do, but a plugin using it on a
  restarted session has to close the session itself. `auto_reject_approvals` is rebuilt from the row's origin.
- **Restart by retried start is a contributor action, while `session/restart` is operator.** The owner's spec has both. A contributor who can see a session can bring it back by retrying
  `session/start` with its id. Bot and sub-agent ids are refused there, so those stay with operators.
- **A session with no run history cannot come back.** That includes one that finished setup, sat idle and then lost its execution. Force close and a new session is the only exit, and a
  retried start gets `rejected`.
- **A stopped sub-agent child now reads as a rejected delegation.** `session_execution_stopped` joined `is_caller_error`, so the parent's tool call fails at once. A Failed child used to
  retry as infrastructure under `session_bootstrap_failed`.
- **A store outage during a terminal LLM failure now calls the provider again.** Writing the error blob failed untyped and ended the workflow. It is now `llm_provider_transient`, so a retry
  re-sends the request within the budget before the run fails. Commit `37793241`.
- **Code tool updates are refused during recovery.** `ready` stays false until the interrupted work is settled, so `session/restart` answers with the true state. A script's call landing in
  that window gets `SessionNotReady`, and the cleanup path reads that as "the holder no longer admits this parent". The window is one or two activities long.
- **`session-restart` has never run against Temporal.** Workflow id reuse with `AllowDuplicate`, a query against the new execution, a parked run across a restart and the emission re-send all
  rest on reading and unit tests. Run `session_restart_live` first at the checkpoint.

- **A stalled session with no due promise stays invisible, as the owner chose.** The reaper reports only what it already looks at: a repair that repeats, and a stopped execution behind an active or queued run. A
  running execution whose workflow task keeps failing, with no overdue timer and no dead-owner promise, logs nothing here. There is no exporter and no describe sweep.
- **`promise_repair_repeated` can warn on a healthy session.** It means a signal sent a pass ago, five minutes earlier, has not landed in the log. A workflow busy for that long before it drains admissions looks the same as a
  stalled one, which is why the text says "may be". A repeat is still signalled, so a truly stalled workflow still gains about 288 signals a day per replica and per promise. I kept sending because stopping would be an automatic
  decision about a workflow that may be merely slow.
- **The promise reaper has no leader.** Every sessions replica still makes the whole pass, and each keeps its own repeat set, so a second replica counts its first signal as new. The planned action left out the judge's advisory
  lock, and memory no longer grows with the fleet, so I left it out too. Reads still scale with open sessions times replicas.
- **A long open session with no checkpoint replays on each pass.** The reaper reads checkpoints and does not write them. A session whose log has none replays that log every 5 minutes, and logs `large authoritative session replay`
  each time once it is large, until a workflow or a gateway read writes one. The workflow normally does, so I do not expect this to last on a live session.
- **Force close of a never-opened session answers `not_found`.** The row is gone and the response needs a session view. It reads like a failure, and the API doc on `force` says so. A retried `session/start` with that id recreates it.
- **A discard can still race an overrun prepare.** Force close stops a running execution and then discards a never-opened row, but an attempt of prepare that outlived its own timeouts may still be inside `start_session`. It could then start a new
  child whose bootstrap creates the row without its origin. It needs an attempt outliving its timeouts, so I accepted it.
- **Sub-agent activities give up after 15 minutes.** A store or Temporal outage longer than that now fails the delegation rather than waiting it out, and the parent's promise fails through the recovery poll. The close on that path
  has the same budget and shares the same outage, so a child can stay open holding a slot until someone force-closes it. The only signal is a warning from the close activity.
- **Every delegation runs one more close activity.** After the parent has its answer the workflow closes the child again on the retrying options, so a close that failed inside resolve does not leave a slot held. For a closed child it is one read, and it keeps the
  execution open for up to 15 minutes in an outage. The parent's next delegation can still hear a limit error in that window.
- **The sweep's universe rotation still restarts with the process.** Each universe keeps its position, but the order among universes is in memory, and the 10-minute budget is shared. With many universes and frequent restarts, the first
  ones in id order get more of the budget. Nothing starves permanently unless the earlier ones always use it all.
- **Migration 012 needs a migrate before the next start.** The runtime refuses a database behind revision 12, so every live stack and the checkpoints need `cargo run -p temporal-runtime -- migrate`, as the branch's purge already implies.
- **The sub-agent default deadline, again.** `SUBAGENT_DEFAULT_DEADLINE_MS` is unchanged and still awaits an owner decision. A child still gets a 1 hour default and a 24 h parent ceiling, which is a default limit against the north
  star. Removing it needs promise and child lifecycle changes beyond this package. Resolved 2026-11-04: `run_timeout_ms` is optional with no default or ceiling, `agent_run` and `agent_spawn` carry no hard promise deadline, and a 15 minute liveness check fails the delegation when the child's session workflow stops.
- **Dead-session details beyond the sweep entry above.** A late event for a closed per-event session now opens a next generation, where it used to end `run_failed`; a closed session counts as dead on every path, and the
  generation entry goes when that successor closes. A config edit on a dead main session rolls forward at once, without waiting for an event, because reconcile reads the status first. A start that landed as queued on an earlier
  attempt, on a session that then died, runs again on the successor, so that event can run twice. A missing row skips the close as specified, so a workflow still running behind it stays up: until the bot closes for a main generation, and for good on a routed session, which teardown no longer knows about. Rotation retires such an
  orphan and this path does not. A sidecar on a closed session keeps requeuing at the busy delay until the sweep or a terminal ends its lane, which is at most 15 minutes of that.
- **A lane parked on its run crosses continue-as-new.** Not asked for by any brief. Deleting the 24 h timer let a lane live as long as its run, and any lane blocked the rollover, so one run lasting days would have held the whole bot's history open until the server terminated the controller. The carry now names the waiting lanes and the next
  execution waits on them again. It is simple but new, it is the only place a controller rolls over with deliveries in flight, and only state tests and compiling prove it. If it misbehaves, reverting `5ef10269` restores the block, and the controller then dies at the history limit on a run of a few days on a budgeted bot, or after about 1,600 deliveries beside one long run.
- **Every close now waits at most 45 s.** This is for the bot activity's 60 s limit, and it applies to public `session/close` too. A non-force close answers `operation_pending` sooner, and a force close terminates a workflow it
  has heard nothing from after 45 s rather than 90 s. A healthy workflow commits the close long before that, but a long stalled workflow task can now be terminated by a force close where it would have made it.
- **A store-path force close re-signals every retained run with a notify intent.** A long-lived bot main session can send up to 256 signals, which is that many history events on the controller. All are dropped by emission id or by having
  no lane. The answer given to the owner says all intents, so this takes it literally rather than limiting it to the runs that close finished.
- **An unknown `session/list` param is now an error.** An external client sending a field the method never declared used to get a list and now gets invalid params, and that includes a Configurator MCP model
  that invents a filter. No caller in this repo sends one. A wider list on an audience filter was the worse failure, so this stays. The other public param types without `deny_unknown_fields` are unchanged, so a misspelled field on them still drops silently.
- **Reference wording is unsigned.** The `session/start`, `session/managed/start`, `session/list`, `session/context/append`, `session/runs/list` and steering text ships with its code and is listed for sign-off under `api-reference-corrections`.

- **A database holding the old bot values refuses migration 013.** It re-adds the two CHECK constraints with validation, so `migrate` stops with a violated check on any `bot_events.outcome` of `run_failed` or `bot_triggers.disabled_reason` of `poll_failed`, `one_shot` or `bot_closed`.
  That includes the `lightspeed_hardening` database if earlier `bots_live` runs left such rows, so purge it before the next checkpoint. `NOT VALID` would have let the migration pass, but an old row would then fail decode inside store-wide bot scans, which is harder to read than a failed migrate.
- **Models can now read camelCase bot values.** The rule stops at tool names, tool arguments, tool failure codes and harness values, but a `bot.reply` receipt's `status` and a bot tool returning an API view carry `BotEventOutcome` as it is, so a bot prompt can show `runFailed`. Refusal codes such as `bot_closed` from `BotRefusalCode` and `not_found` from `api_error_code` stay snake_case, because the model reads them as tool failures. One error is therefore spelled two ways, on two surfaces.
- **External API clients break on every error kind.** `not_found` and every other multi-word kind changed, as did Prometheus labels on connector metrics (`pairingRequired`, `rateLimited`). Nothing in this repo still reads the old values. Outside it, the ls-benchmark fixture `test_agent_run.py:457` needs a follow-up, and any dashboard on those labels needs updating.
- **A stream client no longer sees a call's media on `toolCallCompleted`.** As agreed, media comes from context entries there and from `media` on a read run. `code_workflow_live` now asserts three items on `media`, which nothing has run yet.

- **`rename-vocabulary` went past the listed names, and nothing live has run it.** It also renames the `msc:` creation fingerprint prefix to `dsc:`, the `NativeMcp*` runtime-path types to
  `RuntimeMcp*` (`NativeMcpRuntime` to `RuntimeMcpExecutor`), the bot controller's `ManagedSession` to `ControlledSession` and the web's `showManagedSessions` preference to `showControlledSessions`. That
  preference has no fallback, so a viewer's saved toggle resets once. Every stored event kind, effect kind, RPC method, wire value and live test name here has only been compiled.
- **Migration 014 fails on real data and breaks a running older runtime.** Any `mcp_servers` row still holding `native` fails the new check, and the column rename stops an older binary reading `sessions`. Both
  are accepted for a purged breaking release. `lightspeed mcp put --execution native` and any client sending `native` or `session/managed/start` are refused with no alias. The ls-benchmark repo outside this tree
  may use these names and needs the same follow-up as its fixture.
- **The UI still says Managed.** "Managed by", "Managed sessions" and "Bot-managed session" stay as product copy, because there they only mean controller-owned. "Managed instructions" and "the managed fallback" in
  session settings are a different, older sense, so the word is not fully single-meaning in the UI. Changing that copy is a product call.
- **The registry identity's wire field stays `tool.name` on declarations.** `WorkflowToolSpecInput.name` still means both function name and registry identity, because the brief lists only the harness `ToolSpec.id`. For a function tool the two are one value, and the declared-tool view reports it as `toolId` elsewhere. Worth a look if integrators find it confusing.
- **`as_wire_name` is an intentional escape between the two id types.** It stands for "nothing renames this tool", true of function, MCP and workflow-backed tools today. The adapters also fill `ObservedToolCall.tool_id` with the wire name until `normalize` replaces it from the catalog, as they did before; the type no longer flags that value as provisional.
- **Bot and channel bookkeeping keeps `tool_id` for a workflow tool id.** `BotToolSpec`, `ReceivedInvocation`, `tool_ids` on the channel declarations and `carried_tool_ids` were not in the brief and do not cross the public API. `BotToolInvocationRef` did change because the harness invocation feeds it directly.
- **The ls-benchmark adapter reads a renamed run field.** `~/dev/ls-benchmark/src/lightspeed_harbor/agent.py:875` reads `completedAtMs` from a run view, so `run_completed_at_ms` silently goes empty there until it moves to `finishedAtMs`. That repo is outside this tree and was not changed.

- **No replay check means a wrong `restart-in-place` call shows up in use.** The owner chose no tool. The upgrade doc now says so and gives the
  script's limits, but a third-party bump that changes workflow behaviour prints `restart-in-place`, and the first sign is then sessions stalling as they
  are touched while `/health` stays `ok`. A new namespace heals it. Nobody has run a namespace move: the guide's table of what survives comes from
  reading code and from the live test that terminates both workflows in place of it. Run one on a copy before the first real upgrade.
- **The docs promise some behaviour nothing has shown live.** A real SIGTERM stop of a running stack, the namespace move above, the 15-minute dead-session sweep, a force close
  releasing a waiting lane, a lane crossing continue-as-new, and restart's emission re-send are all stated in guides from code reading and unit tests.
- **Smaller risks found while confirming this list.** They were written under a package's Open or Left notes and not here:
  - A hosted approval cancelled by `CancelRun` or `FailRun` leaves its `mcp_approval_request` in context with no response. Whether OpenAI accepts that on the next request is
    untested, and a refusal would fail each run until the context is compacted.
  - A code tool completion the reducer refuses still ends the workflow rather than the run, because `process_pending` goes through `apply_internal`. Nothing known reaches it, and
    restart now brings such a session back.
  - A config signal landing during a bot's main reconcile is lost: `mark_session_ready` clears the dirty flag for an ensure that used the old config.
  - A lane's status read, steer and append failures end the delivery through `?` with no carried retry, and steering `operation_pending` fails a sidecar delivery after the activity's attempts.
  - `LaneTerminal.failure_message_ref` is carried and never read, so a failed run's delivery summary says `run ended failed` rather than the run's message.
  - An entry removal by a context edit does not clear `entry_guard_attempted`, so a run that made its attempt, had entries removed and crossed 2,000 again fails with no second attempt. The next run heals it.
  - A stored run projection stays `waiting` while live activity says working when one response carries both an await and a hosted approval. It heals at the resume.
  - `execute_call_group` ignores what `drain_pending_admissions` returns, so a drain admitting nothing would spin through that loop. The wait matches what the drain admits.
  - `generation.toolChoice` still offers forced forms in the API and the session editor though some current Anthropic models refuse them. The run fails with the provider's message.
  - `workflow_tool_start_on_call_resolves_via_plugin_worker` changed meaning at checkpoint 1: a session now cancels a finished execution as soon as its reply commits, so a plugin must not rely on a long epilogue after its last reply. The workflow tools guide already says so.

- **A payload that will not convert stalls instead of failing.** At session bootstrap, in the bot
  controller's activities and on the conversation's activity paths, an undecodable result now panics, which
  fails the workflow task and retries it. A fix forward heals the execution, as the owner wanted. Until
  one ships, the execution sits retrying and shows only as workflow task failures, not as a failed
  session or a degraded bot, so an alert on task failures is the only signal.
- **Lenient decoding hides a rename.** Reduced state, its components, and the continuation, conversation
  and bot carries take container defaults, so a renamed field reads as empty instead of failing. A
  checkpoint missing a component used to fall back to replay and now loads with that component empty.
  The shape golden is what catches this at PR time, and only if the author bumps the version when it
  fails. A missing `ready` reads as not prepared and a missing bot `enabled` reads as disabled. Neither
  can happen until a rename; both would look like a quiet stop.
- **Identity-bearing containers do not take a container default, against the brief's wording.** Serde
  needs a whole-struct `Default` for one, and `AgentSessionArgs`, `CreateOrLoadSessionResult`,
  `SessionRecord`, `BotControllerConfig`, `ConversationStart` and `BotTriggerFireArgs` hold ids with no
  honest default. A placeholder id would run real work under a made-up name, so the ids stay required,
  other fields take field defaults, and a test decodes each from its identity alone. A new required
  field therefore fails at PR time rather than being defaulted automatically. `ConversationStart` also
  keeps its other fields required, because it is in the workflow contract schema. Fire args are all
  identity, so they only gained the test.

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

### `store-rejections` (H-13, H-17)

What landed:

- `fcaf2668` (H-13): `SessionStoreError::DataRejected` for SQLSTATE class 22, and
  `is_data_rejection` covering it and `MissingBlobs`. `append_events` and `create_or_load_session`
  return a non-retryable failure for everything but `Store`, a log that will not reduce and
  `SessionBootstrapPayloadTooLarge`. Data rejections carry the type `session_data_rejected`. Fatal and
  bootstrap errors now show the worker's message rather than "Activity task failed".
- `31318c66` (H-13): `validate_workflow_tool_reply` is the one ingest gate, for every resolution with
  a ref or a declared schema, and recovery polls already reach it. Malformed and missing refs fail
  only that promise, and so does a schema that cannot be read or compiled. A joined reply must be UTF-8,
  which closes the joined binary case left open by `promises-and-awaits`. A late reply to a settled
  promise logs at info and an absent one keeps its warning.
- `facb40c0` (H-13): `code_tool_prepare_scope` gets one attempt, 38 s start-to-close and 40 s
  schedule-to-close, and runs through `preparation::await_activity`.
- `b8ddfe87` (H-13, separate so it can be reverted): the drive path discards rejected events and admits
  FailRun with kind Internal and the store's message. A rejection of FailRun's own append, or one with
  no run and no compaction to fail, is fatal. Unconfirmable head conflicts, load and decode errors and
  outages stay fatal.
- `7e80cc7b` (H-17): `encode_event` replaces U+0000 with U+FFFD in string values and keys. The API
  refuses NUL in origin, media name, catalog title and display name on start, managed start and rename,
  as `invalid_request`. The contract and its consumers are regenerated for the field docs.

Calls made:

- See Risky calls for the admission path, ending the workflow on a head conflict, and the gate's cost.
- A broken reply schema fails the reply. The brief left that retryable, but admission validates schemas,
  so the only way to reach it is a bug, and one binding's bug should not block the session.
- Other store variants such as `InvalidRetention` also became non-retryable, since only `Store` can be
  outlasted. A deleted session seen as "disappeared after create conflict" is final too.
- Generic `activity_options()` stays unbounded and only gained a comment, so a long outage still waits
  rather than failing every active session. I did not rename the two same-named helpers, as the brief's
  judge suggested: it would touch about thirty call sites for little.

Deviations from the briefs:

- The sub-agent label is refused in tool argument validation although the package named four fields,
  because it reaches a text column in `subagent_prepare`, which retries without limit.
- The approval note is not refused at the API, as the package did not list it. The codec replaces its
  NUL.
- Joins are not scrubbed: every id in them refuses NUL at construction.
- Channel text and media names before the `bot_events` insert are not checked. A NUL there fails the
  insert as a plain store error. `emit_chat_event` retries it up to 5 times, then the conversation records
  that message as failed and carries on. The manual API returns an internal error. Neither touches a
  session, so it is left for the bots work.
- The judge's `materialize_await_result` and `prepare_joined_context` stay as they are, as it advised.
  After the gate they only see store outages. That closes the detached follow-up stall in Risky calls.

Ported as tests beside the code, each covering its main case: `repro_permappend_verify_z5u9`,
`repro_refute_nulcodec_e9w`, `repro_nul_codec_scrub_judge_q4z`, `repro_refute_wholesys_nul_v6t` and
`repro_replyblobwedge_refute_j8k`, in the behaviour they argued for. Not ported:
`repro_refute_appendwedge_n5g`'s digest case, which belongs to `typed-blob-refs`, and
`repro_refute_replyref_rootguard_j6f`, whose two facts about the root guard the malformed ref test relies
on.

Verified: `cargo test -p harness` (326 lib tests), `cargo test -p store-pg --lib` (14),
`cargo test -p temporal-workflow --lib`, `cargo test -p api`, targeted `temporal-runtime` and `tools` tests,
`cargo check -p temporal-workflow -p temporal-runtime --tests` and
`cargo check --workspace --all-targets --locked`. Clippy and the full workspace suite were not run, as they
belong to checkpoints.

Written but never run, for the checkpoint: `pg_live_a_value_postgres_cannot_hold_is_a_data_rejection_and_writes_nothing`
and the new unstored, malformed and failed-ref rows in `workflow_tool_reply_schema_gates_resolutions`.
The Docker daemon is down. No test drives a store rejection through the workflow to a failed run. Each side
is unit tested, and the workflow step needs Temporal.

Open:

- For `session-restart`: a head conflict now ends the workflow, so restart is what recovers it. A
  request to a session whose workflow failed mid-life is still answered as "failed to start (bootstrap)"
  by `stopped_execution_not_found_error`, and this package makes that more reachable.
- For `typed-blob-refs`: previews that look like refs, and `BlobRef` decoding. The gate's parse check can go
  once a `BlobRef` cannot hold a malformed value.

Independent review of this package:

- Starts at `0a48bd33`, so `fcaf2668` is its first commit.
- A configure or profile operation appended directly, so a typed rejection took the fatal path, or failed an
  unrelated run on the drive path. The store's root gate reads every digest-shaped string, so a caller's
  config could reach this until `typed-blob-refs`. It is now that operation's own failure (`0b857488`).
- The NUL test encoded its entries again before searching them, which scrubs, so it could not see a NUL an
  append let through. It now reads the committed events. A test's doc comment was also on the wrong test
  (`95d8962b`).
- The channel note under Deviations said the insert fails synchronously, which had not been traced. It is
  now traced and corrected.
- Left, for a store that misbehaves: a code tool completion the store rejects still ends the workflow,
  because `apply_internal` treats any refusal as fatal and `process_pending` is outside the failure path.
  Its results hold refs the worker wrote first, and NUL is scrubbed.
- Checked with no change: the drive holds no state between a proposal and its commit, so an admission path
  can keep using it after a rejection; `run_preparation_loop` and `run_dispatch_loop` run beside the session loop and
  neither appends, so racing scope preparation adds no second writer; `load_reduction` falls back to the log on any
  checkpoint or blob failure, so a blob outage cannot read as a final reduction failure; `retain_blob` reports
  a missing blob as `NotFound` on Postgres and on the default path; ids in joins refuse NUL at construction;
  binary `Failed` refs on promises materialize by reference; the workflow, API and TypeScript contracts
  regenerate with no diff.
- Still unverified: everything live, because the Docker daemon is down.

### `typed-blob-refs` (H-18, H-63, H-53)

Landed:

- `40021e98` (H-18): all six `deliver_emission` handlers take `RawValue` and decode through
  `receive_emission`, which treats zero payloads as a decode failure. A failure logs at error level and
  drops the signal. Only the session also sets `last_warning`. There is no `fatal_error`, `last_error`
  or counter. Senders wrap the envelope with `emission_signal_input`, and the wire form is unchanged, so
  typed plugin senders keep working. `BlobRef` decode validates through `parse`.
- `5031cbfe` (H-63): a collecting serializer gathers only `BlobRef` values, recognised by the named
  newtype they serialize as. The store runs it over the decoded event in the append's transaction, and
  the gateway over its typed input. Root tables, foreign keys, the sweep predicate and `MissingBlobs`
  stay, and no refs come back from `encode_uncommitted`. `ToolEffect` gets a typed `refs` map,
  `WorkspaceAttachmentTarget::Snapshot` holds a `BlobRef`, and the workflow intent gets
  `SessionProfileInstructions`. The in-memory store runs both collectors on each event and refuses one
  whose digest the typed walk misses, outside `preview`, `submission_id` and `source.id`.
- `2e882d0f` (H-53): `ScopeOpened` and `ScopeClosed` carry the parent's run, turn and batch joins, and a
  joined `CallDeferred` from a code tool names its tool call id.
- `27113766` (H-18, H-63): a real drive test with a digest as submission id and whole reply, store tests
  for what the in-memory append refuses, and a paragraph in the workflow contract on dropped signals and
  deploy order.

Calls made:

- See Risky calls for dropped emissions, plain-string refs, the append's decode, stored data and older
  heads.
- Workflow emit effects moved `arguments_ref` and `execution_context_ref` into `refs` too. The brief
  allowed relying on the event appended alongside. Moving them keeps one rule for effects and lets the
  cross-check run per event rather than per append.
- `tool_effects_to_api` merges `refs` into `data`, so `ToolEffectView` and the API contract do not change.
- `Attachment::blob_refs` is gone. It included `source.id`, so once that stops being a root, a swept
  snapshot would have made a live attachment look unavailable and failed its edge recording. Admission
  and edges use only `content_ref`.
- A non-canonical `snapshotRef` or profile `blobRef` is now `invalid_request` at the API edge, because
  those are where a string becomes a ref.
- A closing scope whose parent cannot be found keeps empty joins and still closes.
- `BlobRef`'s explanation is a plain comment, not a doc comment. A doc comment turned it into a named
  schema definition and churned the whole workflow schema.
- The reaper test's malformed log uses bad joins, since a store now refuses an undecodable event. The
  `session_content` test no longer has foreign events, so its claim that they are skipped undecoded went.

Deviations:

- H-18's brief says to count failures. The package instructions say no counter, and those were followed.
- `new_unchecked_for_tests` was already gone.
- The judge's sender-side suggestion, tightening `sourceResolutionEnvelope` and `parseEmissionEnvelope`
  to `promise_<number>`, was left out as outside this package's crates. The review below added it.
- The dead re-validation also went from `CodeExecutionDescriptor::validate` (with its error variant), the
  code execution context and the reply gate, which the earlier package's note anticipated.
- The gateway still answers `internal` when a typed input ref has no blob, for example a `snapshotRef`
  naming nothing. Only free-text digests were in scope.

Ported as tests beside the code, in the behaviour they argued for: `repro_blobref_trace_v4k7` (decode and
attachment cases), `repro_fu_h63chk_v9k`, `repro_storagecodec_zq9` and `repro_refute_appendwedge_n5g`
(digest cases), `repro_refute_emissiondecode_g7h`, `repro_poisonsignal_refute_h4v` and
`repro_scopejoins_refute_y2g7`. The adversarial walk fuzz was not ported.

Verified: `cargo test -p harness` (343 passed), `cargo test -p store-pg --lib` (14), `cargo test -p
temporal-workflow` (196, plus 3 contract tests), `cargo test -p temporal-runtime --lib` (490, 1 ignored),
`cargo test -p temporal-runtime --test fake_loop` (2), `cargo test -p tools --lib` (318), `cargo test -p
api-projection --lib` (46), `cargo test -p llm-runtime --lib` (199), `cargo test -p test-support` (13), and
`cargo check --workspace --all-targets --locked`. The workflow contract exporter changed only
`workflow-contract.md`. Clippy and the full workspace suite were not run, as they belong to checkpoints.

Written but never run, for the checkpoint: the garbage-signal step in
`workflow_tool_reply_requires_exact_stored_producer`, the rewritten fixtures in `store_pg_live`,
`session_lifecycle_live` and `audience_live`, and the digest-in-preview step in the blob root test. The
Docker daemon is down.

Open:

- For the docs package: `session/events/read` scope events now have joins. A grep of the guides found
  nothing describing roots by string shape, so H-63 appears to need no correction there.
- Any failing test in the `temporal-runtime` lib binary aborts it on this machine with "failed to initiate
  panic, error 5" rather than reporting the failure, so a parallel run names no test. Running tests
  one at a time with `--exact` finds them. I did not check whether it predates this package; nothing here
  touches panic handling.

Independent review of this package:

- `session_instructions_from_api` sat after the test module, which the CI Clippy command rejects. It moved,
  and a live test's doc comment that the new helper had split off went back on its test (`4e6a2958`).
- The walk left 128-bit integers to serde's default, which is an error, so such a field would have refused
  every append of that event. Nothing has one today (`923d9b87`).
- That `RawValue` survives garbage now has a test as well as a reading: the default converter, called as the
  workflow macro calls it, takes no payloads, bad bytes and too many payloads. The live step is still unrun.
  `prepare_payload_context` also has a direct test that a bad `content_ref` drops that one attachment
  (`dffcb67c`).
- Dropping a malformed emission made the helper's gap matter, because a plugin passing `reply` as its promise
  id now hears nothing. Both TypeScript helpers refuse an id that is not `promise_<number>` (`b4a46ce9`).
  This goes past the package instructions, and a non-TypeScript sender still gets only the drop.
- Checked with no change: all six handlers and every sender; the worker uses the default converter; no
  production writer appends a foreign event kind, so the append's decode cannot refuse one; no `ToolEffect.data`
  writer still holds a ref; the harness has no plain-string `_ref` field or custom serializer hiding a ref;
  joins hold only ids, so the walk losing them loses nothing; the joined deferral's id and scope joins are
  outside what the reducer reads, and the drive test replays them.
- The string refs in bot and channel workflow types (`document_ref`, `tools_ref` and others) are not in session
  events, so this package does not touch them. Bot event roots are their own path.

### Live checkpoint after `typed-blob-refs`: not run

The live suites have still not exercised this branch. The Docker daemon is down, so the dev stack's Postgres
(`localhost:15432`) and Temporal (`127.0.0.1:7233`) are absent. Docker Desktop's logs show an orderly shutdown at
02:12 local on 2026-11-03, with a CLI client connecting just before it stopped, so it does not look like a crash.
Starting it stays out of bounds for this run.

- `live-reset.sh` could not reach Docker, and its `migrate` step ended in `pool timed out`, so the hardening
  database and namespace were not recreated.
- `runs_live` failed 10 of 10 and `mcp_live` 5 of 5 with `pool timed out while waiting for an open connection`.
  `subagents_live` failed 9 of 9 with `tcp connect error` on `127.0.0.1:7233`. None got as far as a workflow, so
  this says nothing about the code, and no test or code changed.
- The three suites compile, with `cargo test -p temporal-runtime --test runs_live --test mcp_live --test
  subagents_live --locked --no-run`.

Every "never run" and "written but never run" note above is still open. This checkpoint was meant to be the first
real proof of the workflow changes, so do not treat them as proven. With the stack up, run `live-reset.sh`, the
three suites, then the tests the earlier notes name.

## Checkpoint 1, after typed-blob-refs (2026-11-03)

Static: clippy with warnings denied passes, `cargo test --workspace` passed 2,759 tests, and TypeScript typecheck and consumer tests pass.

Live, on the dev stack in a separate database and Temporal namespace: `store_pg_live` 42, `session_lifecycle_live` 5, `audience_live` 1, `runs_live` 10, `sessions_live` 16, `mcp_live` 5, `subagents_live` 9, `workflow_tool_plugins_live` 17, `code_tools_live` 4 and `code_workflow_live` 15 all pass.

Failures found and fixed: the cleanup of the session store dropped the two lines that clear close markers on reopen; a reply schema test built a malformed ref that decode now refuses; a retention test expected the old error variant for a value Postgres cannot hold; and clippy found two lints.

One test changed meaning. `workflow_tool_start_on_call_resolves_via_plugin_worker` asserted the started execution was still running after its joined reply. Transport work now goes out at each commit, so the session cancels an execution it has finished with before the test looks. The cancel rule itself is unchanged from main, only its timing. The test now checks the call finished well inside the execution's 60 second hold.

A failing test in `workflow_tool_plugins_live` can abort the whole binary with `failed to initiate panic, error 5`, which hides later tests and needs `--nocapture` to read. It predates this work and was not investigated.

## Progress after checkpoint 1

### `run-limits-and-failure-kinds` (H-59, H-92, message part of H-25)

Landed:

- `c5d048ea` (H-59): `ActiveRun.turn_count` is incremented in the `Turn::Started` arm and never pruned.
  `terminal_run_proposal` compares it with the run's `max_turns`, then the session's, wherever the run would need
  another turn, and fails with `LimitExceeded` and a new engine message. Unset is unbounded. Admission refuses 0 on
  session and run config. `maxToolRounds` is gone everywhere, with the contract and TypeScript consumers regenerated.
- `84fbead5` (H-92): the kinds are now `ContextFailure`, `ModelFailure`, `RequestRejected`, `OutputTruncated`,
  `ContentFiltered`, `LimitExceeded` and `Internal`, changed together with the view and the projection mapping.
  Truncation and filtering are read from `facts.finish` at the terminal proposal, which covers both the status in-tree
  adapters use and the `Succeeded` arm. A provider-cancelled generation on an active run is `ModelFailure` with its own
  engine message. Every failure holds for steering, and only a requested cancel drops it.
- `0defdd1e` (H-25, message only): `context_limit_failure_ref` replaces the false runtime-boundary text on a context
  failure with no message. The H-25 renames stay in `rename-wire-identities`.

Calls made:

- See Risky calls for what the limit counts and when it fires.
- `RunFailure.message_ref` stays an `Option`, so older-shaped writers still decode, but every producer now fills it. A
  turn result with no message gets a constant: `OutputTruncated` and `ContentFiltered` have their own, `ModelFailure`
  and `RequestRejected` use the existing runtime-boundary text, whose documented purpose this is.
- The projection's last resort, for a message it cannot read back, is a fixed sentence and not the kind name.
- The failed overflow compaction path in `drive.rs` also uses the context-limit constant. It had no fallback, and the
  brief only named the planner's arm.
- A run out of turns and a provider-cancelled turn skip proactive compaction during the steering hold, like a failed
  turn, since nothing will use the room.
- Web and CLI lead with "The response was cut off" and "The provider refused the response" for the two new kinds.
  `limit_exceeded` keeps "Run failed:", because its message already says it all.

Deviations:

- Eval and live-test limits that would bind were removed rather than raised, since none of those tests is about limits
  and unset is the product default. Eval no longer has default caps of 12 and 8, and keeps its `max_steps` ceiling.
- A run limit of 0 reaching the gateway is refused by workflow admission like `input_limit_tokens: 0`, not at the API
  edge. A session limit of 0 is `invalid_request` at the edge through `SessionConfig::validate`.
- The answer on the card suggested `turns.len()`. The package's `turn_count` replaces it, with a test that drops
  settled turn records.

Ported as tests beside the code: `repro_naming_vocab_q8w` (limits, override both ways, unbounded, last-turn answer, next
run from zero) and the provider-cancelled case of `repro_runlife_more`, whose old assertion wanted `Cancelled` and now
asserts the agreed `ModelFailure`. `repro_fu_h59grow` measures state growth and belongs to `state-pruning`.

Verified: `cargo test -p harness` (360 passed), `cargo test -p api` (103, plus 9 contract), `cargo test -p api-projection
--lib` (47), `cargo test -p temporal-workflow` (197, plus 3 contract), `cargo test -p temporal-runtime --lib
gateway::service` (154), `cargo test -p test-support` (13), `cargo test -p eval` (6), the CLI failure message test,
vitest on the config editor, transcript and run section, `npm run typecheck`, and `cargo check --workspace --all-targets
--locked`. Clippy, the full suite and live tests were not run, as they belong to checkpoints.

Written but never run, for the checkpoint: the live fixtures that lost their limits in `code_model_live`, `mcp_live` and
the eight `llm-runtime` live suites, and the eval cases. Nothing has driven a turn limit through Temporal yet, though it
is the same planner path as other failures.

Open:

- `state-pruning`: `terminal_run_proposal` reads only the latest turn record and `turn_count`, so pruning settled turns
  is safe for the limit. The latest turn has to survive, and `facts.finish` on it decides the failure kind.
- `rename-casing`: `limit_exceeded` appears in the `LimitsConfig.maxTurns` doc, and the web's `RUN_FAILURE_LEADS` keys
  and transcript tests hold the snake_case values.
- `docs-and-review-notes`: a grep of `docs/documentation` found no `maxToolRounds` or failure kind lists to fix. The
  guides do not yet say that `maxTurns` is enforced or how.

Independent review of this package: the planner paths, generated artifacts and adapter finishes hold up. Tried and
found no wedge: a queued manual compaction at the limit, succeeded and failed, and steering while parked on an approval
at the limit. Added coverage for the claims that had none: an overflow at the limit now also checks the next run's
request, a decided hosted approval reaches the next run's request, and an approved native call runs before the limit
ends the run. The wire breaks from this package's report were missing from Risky calls and are there now. Steering that
lands during a tool turn at the limit still sits between the call and its result in context, as it does without a
limit; that is H-58, for `context-render`.

### `state-pruning` (H-57, H-76)

Landed:

- `caab1e95` (H-57): at turn and batch completion a run keeps its latest turn and any turn with an open batch, and an
  older turn goes with its completed batch. Terminal approvals go at the same boundaries, except a native decision of
  the open batch, which covers `OpenAiMcp` records with no batch id. The last 64 steering batches stay and an older one
  goes only once consumed. The last 256 terminal run records stay, with `RunQueueState.pruned_through` and `is_pruned`
  for readers. `wait_for_approval_decision` treats a vanished record as decided.
- `278f71e1` (H-57, H-76): `prune_finished_invocations` and `prune_terminal_promises` run at batch completion and run
  terminal. A closed scope leaves with its parent, replacing the current-run filter in `ScopeOpened`. Absent ids became
  no-ops at admission: resolutions, cancel and detach effects at or below the cursor, delivery and start failures, and
  scope close. `MAX_IN_FLIGHT_WORKFLOW_TOOL_INVOCATIONS`, `in_flight_invocation_count` and `in_flight_limit_message`
  replace the lifetime count, and `MAX_CODE_TOOL_SCOPES_PER_RUN` is gone. The host now reads each promise event right
  after applying it, in `apply_appended_entry`.
- `297fb883` (H-57): `SessionStore::read_run_record_entries`, on both stores, reads a run's accepted, started and
  terminal events by kind across fork segments. `run_record_from_entries` and `run_usage_from_entries` rebuild from them.
  The new `gateway/service/pruned_runs.rs` fills `session/runs/list`, and run lookups, cancel and steer fall back through
  it or `is_pruned`. The bootstrap budget comment is corrected. API and workflow contracts and TypeScript consumers are
  regenerated.
- The commit after it only corrects a comment in the bound test.

Calls made:

- Run records are pruned by terminal order and not by run id, so the newest terminal record is always there for the
  notify lookup. Ids in the window can therefore be out of order when a queued run is cancelled behind an active one.
  The merge in `project_run_summaries` sorts by id and lists such a run once.
- The store method takes a run-id range and no row limit. Run ids count from one with no gaps, so a page of runs is a
  range of ids and can never split a run's events. It also reads `RUN_STARTED`, beyond the brief's accepted and terminal
  kinds, because that is the only source of `started_at_ms`.
- A page reads one more run than it can hold, and `has_older_runs` is then just "more than a page". That stays true
  across the boundary with no separate count.
- `session/read` never consults the log. A session that has pruned holds at least 256 records and that page is at most
  100, so its `hasOlderRuns` is true from state alone.
- Terminal promises were kept by highest id here, which the review below changed to settling order. The receiver cancel
  and the detached follow-up moved to per-entry reads with the execution cancel, and they stay there because the
  invocation record still goes at the boundary.
- A notify invocation is counted by neither the worker nor admission, even when several share a batch, so the limit
  covers only work the run waits on.
- The guard "never prune a promise the open batch references" is structural and has no code. Promise pruning only runs
  at batch completion and run terminal, where no batch is open, and the doc comment says so.
- Cancel and detach effects on a pruned id are no-ops. The brief named cancels, and detach sits beside it with
  the same reasoning.
- A code-tool await on a pruned id now fails that call with the await-rejected text, matching the model's await.

Deviations:

- The "two harness tests that depend on turns accumulating" did not show up: `cargo test -p harness` stayed green after
  turn pruning, probably because the earlier `turn_count` work changed them, though I did not trace that. The tests that
  did change: one code-tool test asserted a scope still held after the run ended, and the emissions test in `storage.rs`
  asserted records still held.
- The state bound is 24 KB in the new one-call test against the brief's 35 KB, because its turns carry no reasoning
  entry. It is asserted below 45 KB. Code mode is about 178 KB and is asserted below 200 KB. See Risky calls.
- `run_submissions`, named by the card, was already gone.
- The size and growth reproductions are ported as `crates/harness/tests/state_bounds.rs`, an integration test because it
  drives whole sessions through the public API: thousands of turns in plain and code mode, provider approvals, steering,
  run and promise windows, in-flight counting, scope lifetime and log rebuild, each replayed to the live state.
  `repro_refute_emissioncap32_u7c` is ported as its in-flight counting test. The compaction-trigger experiments in
  `repro_fu_h57_r4w` and `repro_fu_h57_c8m` compared options and have nothing left to pin.

Verified: `cargo test -p harness` (362 unit, 15 in `state_bounds`), `cargo test -p store-pg --lib` (14), `cargo test -p
api` (103, plus 9 contract), `cargo test -p temporal-workflow` (200, plus 3 contract), `cargo test -p api-projection --lib`
(47), `cargo test -p temporal-runtime --lib` (495), and `cargo check --workspace --all-targets --locked`. One narrow live
test ran, because the new SQL had no other check: `pg_live_run_record_entries_are_read_by_run_id_across_a_fork`, against
`lightspeed_hardening`. Clippy, the full suite and the other live tests were not run, as they belong to checkpoints.

Not exercised: nothing has driven pruning through Temporal, so a long run across continue-as-new is proven only by the
reducer tests. `project_loaded_run`'s log fallback needs a `PgStore` and a client, so its helpers are tested and the
method itself is not.

Open:

- `format-guards`: `RunQueueState.pruned_through` is new in checkpoints, and the checkpoint version was not bumped here.
- `docs-and-review-notes`: the guides do not yet describe the 256-run dedupe window, old run reads from the log, or the
  in-flight limit.
- `temporal-runtime` unit tests abort with `failed to initiate panic, error 5` when a test panics on this machine, so a
  failure shows no message. `--test-threads=1` at least names the test. It looks unrelated to this package, but I did not
  check it against an earlier commit.

Independent review of this package. Turn, batch, approval, run record and scope pruning, paging across the boundary,
per-entry host reads, H-76 and generated artifacts hold up. Tried and found no wedge: a waiting script call at a
boundary (`blocks_parent_resume` and run-end interruption mean none reaches one), events after `BatchCompleted` in one
append, and ids allocated from cursors rather than counts. Fixed:

- `b69882ca`: promises were windowed by highest id, so a job pending while 256 newer promises settled left state the
  moment it settled, before the model could await it. At one `code_execute` per turn that takes about 256 turns, which
  loses a result in the kind of run this package exists for. `PromiseComponentState.settled` now holds settling order.
- `9b36e730`: execution cancel read a pruned sibling promise as still open, so that execution was never cancelled. A
  push notify whose delivery was given up on only reached an info log, and now sets `last_warning`.
- `346cf28a`: the steer waiter compared `steering.len()` to a baseline, and pruning holds that count level from 64
  batches, so an accepted steer could time out with an internal error and be retried into a double injection.
- `bfe5a583`: the scope query answered every absent id as closed, which made the runner's lost-receipt path in
  `finalize` dead, so a delayed prepare could open the scope again. The query rejects again; close and the cleanup loop
  handle an absent scope themselves. Risky calls is corrected.
- `6526b602`: the `submissionId` API doc still promised a retry always returns the original run.

### `llm-provider-errors` (H-34, H-15, H-88, H-49, H-54)

Landed:

- `2fb062fd` (H-34): `HttpClient::transport_error` replaces all four `map_reqwest_error` copies. Retryable is
  timeout, connect, request or decode, since reqwest reports a failed body read as a decode error. A request reqwest
  refused to build stays terminal. `tests/transport_failures.rs` runs a closed connection, a stalled body and a
  refused connection through every client's real call against a local listener.
- `129161b0` (H-15): the Anthropic client passes an explicit `context_length_exceeded` code for an
  `invalid_request_error` saying "prompt is too long" or "exceed context limit". `tests/provider_failures.rs` sends
  the real 400 through a client and a listener, and sees `ContextLimit` at the runtime and chunk splitting in rolling
  compaction.
- `2e281a2c` (H-15): images cost a ceiling from `MAX_IMAGE_SIDE` (5,333 tokens, no blob read), PDFs bytes/8, the rest
  bytes/4. The pre-send check only shrinks while a smaller cut exists, and an estimate cannot refuse the first chunk.
  Reported usage still enforces `MAX_INPUT_TOKENS`.
- `d59efd66` (H-88): `LlmAdapterError::InvalidProviderResponse` for everything read off a response.
  `fail_on_invalid_response` turns it into a `Failed` result with usage and response id, so the debug dump step runs
  and the run fails as `ModelFailure`. A harness test with replay pins that path.
- `65cc644a` (H-49): the ten constants moved to `llm_clients::content` with unchanged values. The three compaction
  kinds stay, and no neutral kind was added.
- `408a0116` (H-54): `document_entry` takes the entry and requires a user message with no provider kind. The Anthropic
  summary preview is cut to 256 characters, and the `preview` field doc is corrected.

Calls made:

- The classifier is a method on `HttpClient`, because it already holds the timeout. The timeout message now reads the
  same everywhere, including reqwest's own text, which audio used to leave out.
- A refused compaction attempt no longer counts as spent, beyond the brief. Otherwise a whole prefix estimated near the
  budget that the provider refuses for free would have used up the budget for its own halves.
- Compaction's "no complete usable summary" and "did not reduce its input" became `InvalidProviderResponse`, since
  they judge what came back. Its budgets and limits keep `InvalidProviderRequest`.
- On Anthropic the pause_turn loop now falls out to the same path, so usage is summed over continuations, including a
  turn that passes the 8-continuation cap. That cap used to return with nothing kept.
- The rule in `document_entry` covers PDFs as well. The judge left PDFs on media type alone, but the package text
  applies it to the function, and every admitted PDF is a user message with no provider kind.
- The harness's own test now spells `"openai.responses.message"` as a literal, because the harness does not depend on
  `llm-clients`.

Deviations:

- The executor unit test keeps a hand-set kind and only takes the real message. The flow through classification is
  tested in `provider_failures.rs`, because `parse_provider_http_error` is private to the client.
- Compaction responses that cannot be read keep the new error name but not their usage: the existing failed compaction
  result reports none. `reject_failure_finish` on Completions is a provider-reported failure on a 200 and is untouched,
  so it also keeps no usage and no dump.
- Ported from the old reproductions: media, text and budget cases into `compaction.rs`, and identity shapes into
  `provider_failures.rs`. Not ported: the session journeys over reducer planning, whose compaction side is now covered
  by the unit tests, and the validator checks, which pinned the grammar and nothing that changed here.

Verified: `cargo test -p llm-clients -p llm-runtime`, `cargo test -p harness` (363 unit, 16 in `state_bounds`), `cargo
test -p api-projection --lib` (47), `cargo test -p api --tests`, `cargo test -p temporal-workflow --test
workflow_contract`, `cargo check --workspace --all-targets --locked`. The workflow contract exporter leaves no diff.

One live test ran, because the real wording could not be checked otherwise:
`anthropic_messages_live_runtime_reports_input_overflow_as_context_limit`, against the real API on `claude-opus-5-5`.
It sends about 12 MB and confirms "prompt is too long" reaches the runtime as `ContextLimit`. Nothing here has run
through Temporal, so overflow recovery after a real 400 and a failed result with usage are covered by unit tests only.

Handoff:

- `oversized-input-refusal`: "the smallest complete compaction chunk exceeds the context window" now means the provider
  confirmed it, so that is where to name the largest entry. An entry that can never fit still ends as `ContextFailure`,
  and classification does not rescue it.
- `docs-and-review-notes`: no guide text changed here. The compaction guide should say the size estimate is advisory if
  it describes it at all.

Independent review of this package. All five items hold up against their briefs: the classifier replaces every copy and
`Response::chunk` reports through the decode kind too, so audio is covered; every response-side error reaches
`fail_on_invalid_response`; no reader of the summary preview needed the full text; every admitted document is a user
message with no provider kind; the workflow contract exporter leaves no diff. Fixed:

- `a29ed3e1`: the pre-send budget check was skipped only while nothing had been spent, so an estimate could still refuse
  any later chunk. Two 10 MiB PDFs estimate at 2.6M tokens and a retained tail counts in every chunk, so every compaction
  with more than one chunk failed before the provider was asked. The check is gone: the budget is enforced after each
  call from reported usage, with the estimate standing in only when a backend reports none. The `2e281a2c` entry above
  describes the first version. Risky calls now also records the tail PDF case and the timeout message.

### `compaction-triggers` (H-77, H-78, H-85, H-50)

Landed:

- `54d47eb1` (H-50): the catalog update header no longer says the earlier version is "above". The adapters do not change.
- `cae019dd` (H-77): `ContextCompactionState.failure_floor` holds the estimate and the active entry count of the last
  failed proactive attempt, set by the reducer and cleared by any success. Either trigger waits for about 10% growth past
  its floor. The last generation's count no longer fires once a compaction has finished after that turn was planned. The
  planner function is split into `next_compaction_proposal` and `high_watermark_compaction_proposal`, and the floor sits
  only on the second. Manual compaction and overflow recovery are not gated.
- `5f3ea158` (H-78): proactive compaction needs an active run, so it fires at the next run's start, before its input is
  written. Manual compaction still runs when idle.
- `fb5a1c7c` (H-85): the entry count triggers at 1,500 active entries, after the enabled check and before the policy
  gate. The snapshot input stays.
- `7402f301` (H-85, the guard, revertable alone): at a turn boundary with 2,000 or more active entries, one compaction
  attempt per run that ignores the floor, otherwise the run fails as `ContextFailure` with the new engine blob
  `ENTRY_GUARD_FAILURE_CONTENT`. `ActiveRun.entry_guard_attempted` records the attempt. Public API text regenerated.

Calls made:

- The estimate travels on the plan as `ContextCompactionPlan.estimated_tokens`. The request moves the revision, so the
  observed count is gone by the finish and the reducer could not read it back. A recorded plan is already a fact.
- A plan whose run has ended, or is being cancelled, records no floor, because that says nothing about whether
  compaction works. This covers `compaction_abandoned_proposal`, which finishes as Failed.
- The token floor is `None` when an attempt fired on the entry count with no estimate. The next token crossing can then
  try once, and that failure sets it.
- Both triggers keep the `HighWatermark` wire value. "Proactive" means that value, and a new one would have added an API
  and client change for no behaviour.
- The attempt flag is set for any `HighWatermark` request at 2,000 or more, not only the guard's own, since either one is
  the attempt that decides the run.
- The flag clears when a success lands under 2,000, beyond the brief. Otherwise a very long run whose compaction works
  would fail the second time it reaches the limit. A success that stays over keeps the flag, so the run fails rather
  than re-summarizing at every boundary.
- The entry trigger also needs a summary to be able to get the count back under, beyond the brief's "no prefix". A
  retained tail of 1,500 would otherwise compact at every boundary with no failure for the floor to catch. The guard
  does not have this check, because it is bounded by its one attempt.
- An active run is the only case tested for H-78, not "active or queued". The run planner starts a queued run before the
  context planner looks, so a queued run with none active cannot reach it.
- `CompactionPolicy::Disabled` gets an API doc saying what 2,000 entries does to it, and `ContextFailure` is widened in
  both the harness and the API text. The guard's limit is not exposed as config.

Found on the way:

- The context planner wrote a fresh run's input while a compaction was pending, which moved the revision under the frozen
  request and made its result refuse. It was reachable before, by a manual compaction landing right at a run's start.
  H-78 made it the normal path, so a pending compaction now holds that planner. Fixed in `5f3ea158`.

Deviations:

- No overflow exception in the guard. I wrote one and removed it after tracing: once the attempt is made, the run fails
  before another turn can start, so an overflow with the flag set cannot occur, and an overflow without it goes to
  recovery first because the context planner checks it before the watermark.
- Not ported from the old reproductions: `every_turn_boundary_compacts_when_the_floor_is_above_the_threshold`, because the
  owner accepted that churn on low-threshold sessions when choosing failure-only backoff. It is unchanged. Ported as
  regression tests beside the planner: the failing-compaction, stale-fallback, steering, run-end and mid-run cases.

Verified: `cargo test -p harness` (383 unit, 16 in `state_bounds`), `cargo test -p temporal-workflow --lib` (201) and
`--test workflow_contract`, `cargo test -p api --test schema_artifacts`, `cargo check --workspace --all-targets --locked`.
The workflow contract exporter leaves no diff. No live test ran.

Handoff:

- `docs-and-review-notes`: the compaction guide should say that proactive compaction waits for a run, that 1,500 active
  entries triggers it on every enabled policy, and that a failed attempt backs off by growth. It should also describe the
  2,000-entry guard, what it does to a Disabled session, and that manual compaction is the way out. The web demo's
  simulated compaction in `platform/web/src/demo/engine.ts` was not reviewed against the new timing.
- `format-guards`: `failure_floor`, `estimated_tokens` and `entry_guard_attempted` change the stored state and the
  requested event, for the checkpoint bump and the shape tests.

Independent review of this package, with every call path traced for wedges. The planner gaps it checked held: the run
and context planners agree on which guard states are terminal, and a guard state where no compaction is proposed always
has a terminal or a turn next. A parked run, a cancelling run and an unstarted tool-call turn are not boundaries. The
projection refresh at a run's start does not move the revision when nothing changed, so the observation survives to the
next run. Standalone compaction is not restricted by policy in any adapter. Regenerating every public consumer and the
workflow contract leaves no diff. Fixed:

- `e05445d5`: a session whose retained tail held the count at 2,000, such as one with context appended after its last
  turn, had a successful attempt that stayed over the limit, then a failed run with no turn to roll the tail out. Every
  later run did the same, and manual compaction covered the same prefix. At the limit a compaction now covers the whole
  settled window, as the second overflow attempt does. A regression test covers it. Risky calls now also records this
  and the stopped run's input.

Still open, not fixed: an entry removal by a context edit does not clear `entry_guard_attempted`, so a run that made its
attempt, had entries removed and then crossed the limit again fails without a second attempt. The next run heals it.

### `run-capacity-and-model-switch` (H-28, H-80, H-79)

Landed:

- `c663c3ac` (H-79): `validate_retained_native_model` is gone, so any model on the route is allowed for a run or a document,
  whatever context holds. Provider and API kind stay pinned, in admission and in the reducer. No guard replaces it. The
  `model` and `RunStartConfig.model` API docs say what a switch does. Contracts regenerated.
- `f3d11b39` (H-28, H-80): `input_limit_tokens` sits beside `run_config` on `RunRequestCommand`, `AcceptedRunEvent`,
  `AcceptedRun` and `ActiveRun`, outside equality and the submission digest. `SessionToolsetRequest.input_capacity_model`
  names the model and `SessionToolsetPreparation.input_limit_tokens` carries the answer, which the `PreparedRun` branch sets
  on the command. `ContextConfig.reported_input_limit_tokens`, `RunConfig.input_limit_tokens`, `resolve_context_capacity`
  and the gateway's write on override runs are deleted. The zero check moved to run admission as `Invalid`. Contracts
  regenerated; the workflow contract did not change.

Calls made:

- The instruction reads "the active run's field, then config", which alone would let a reported 200k beat a declared 100k.
  Preparation therefore does not ask about a model the document declares a limit for, so the order is correct as written.
- The document's limit still applies only to the document's model, as the existing test wanted. An override run on another
  model with no reported figure has no limit, rather than borrowing one meant for a different model.
- The activity skips discovery when `input_capacity_model` is `None`, which every non-run preparation sends. Discovery goes
  after the toolset, so a refused toolset does not pay for it.
- The universe shares one `ModelDiscoveryService` between the gateway and its activities (now an `Arc`), so a credential
  change invalidates both. Only `UniverseRuntime` wires it, and that is the production path.
- A retry whose submission is known skips preparation and arrives with `None`, which is fine because capacity is not part of
  the comparison.
- `ContextCompactionView.inputLimitTokens` gained a doc comment saying where the figure comes from.

Deviations:

- `ActiveRun` carries the field too, which the instruction implies but does not list.
- The web composer's copy is unchanged. "Applies to each message you send from here" became true when the pin went.
- Old reproductions not ported: `reported_limit_breaks_document_identity_and_profile_apply_drops_capacity` and
  `carrying_the_value_forward_keeps_identity_and_capacity` test a field that no longer exists. Ported as regression tests:
  retries in all three run states, a replaced document keeping proactive compaction, the context-limit fallback, the
  override, edit and queued model cases. The retry, proactive and model-switch tests also assert replay.

Verified: `cargo test -p harness` (389 unit, 16 in `state_bounds`), `cargo test -p temporal-workflow --lib` (203) and `--test
workflow_contract`, `cargo test -p api --test schema_artifacts`, `cargo test -p api-projection`, the `models_api` and
`gateway::service::tests` filters of `cargo test -p temporal-runtime --lib`, `npm run typecheck`, `cargo check --workspace
--all-targets --locked`. No live test ran.

Handoff:

- `docs-and-review-notes`: `using-lightspeed/sessions-and-runs.md` (near line 213), `models-and-credentials.md` (near line
  100) and `profiles-and-instructions.md` (near line 151) still say retained state can prevent a model change. Reword them
  from the owner's answer. `how-it-works/context-and-storage.md` near line 193 should say reported capacity is resolved per
  run, and a discovery failure uses the context-limit fallback for that run.
- `format-guards`: `input_limit_tokens` moved on the `runAccepted` event, on stored queued and active runs, and off `RunConfig`
  and `ContextConfig`, for the checkpoint bump and the shape tests.

Independent review of this package, with the capacity and model paths traced. The ordering call holds: a document cannot
change while a run is active or queued, or while one is being prepared, so a run's reported figure cannot outlive the
document or model it was resolved for, and "the run's field first" never beats a declared limit. Every production
`RequestRun`, the gateway's and the detached follow-up, goes through `PreparedRun` unless its submission is already known.
Discovery cannot fail the preparation activity: the toolset is store reads, so it and the 15 s discovery bound fit the 30 s
activity limit, and the singleflight lock adds no second wait. A compaction request is derived from state and a run cannot
start while one is pending, so the capacity in its fingerprint cannot move under it. No store query, fixture or adapter reads
the moved fields. Regenerating every public consumer leaves no diff. The full `temporal-runtime` lib suite also passes (495),
which the implementer had only filtered. Added:

- `run_capacity_live`: nothing automated reached the activity wiring, and a lost wire would quietly stop proactive
  compaction. One narrow test against the dev stack and real Anthropic shows a positive limit after a session's first run
  with nothing declared, then completes a run on `claude-sonnet-5-5` and another back on `claude-opus-5`. It passed. It does
  not force retained thinking across the switch, so it proves the path, not reasoning continuity.

No code changed. The idle view and idle manual compaction notes under Risky calls stand as written.

### `context-render` (H-58)

Landed:

- `4420f71e` (H-58): `planned_context_entry_ids` renders through one new pass, `tool_groups_adjacent`. Results follow their
  calls and anything else arriving inside a group goes behind its last result. The log keeps arrival order, and the reducer
  makes no ordering claim. `context::plan_next` also holds steering while a batch is open, so a steer lands after
  `BatchCompleted` and before the next `Turn::Started`, in the log as well. `turn_in_flight` is not widened.

Calls made:

- The calling turn's own output stays in place, including text and reasoning after its first call. Holding it back would
  break Anthropic's signed thinking between calls.
- A later turn's output closes a group that was never answered, and whatever was held goes out ahead of it. A removed result
  therefore leaves arrival order, which is what the next request already carried. A result answering no open call is an
  ordinary entry.
- A media entry counts as "anything else". It goes behind every result of its group, after any note that arrived before it.
- Compaction needed no code of its own. `standalone_prefix_ids` already reads the planned order, covered entries go by id, and
  a summary renders first, so nothing assumes a stored prefix. `safe_cuts` sees the rendered order.

Deviations:

- "Replay vectors" means log replay assertions in the new tests. The crate has no vector format and no new fixture was added.
- The Completions lowering repair proposed in the source finding was not built, because the owner chose render order.
- `steering_in_the_started_window_of_a_tool_turn_stays_uncompactable_until_carried` asserted steering materializes before the
  batch. It now checks the same exclusion in the real window, after the batch and before the next turn.

Tests: order assertions in the steering and parked-await tests, new drive tests for steering during generation and during a
native approval wait, appends during a batch and during that wait, media of parallel calls, a compaction request and a
compaction cut, all with log replay except the cut. `assert_tool_calls_answered` no longer excuses a media entry inside a group.
Seven unit tests cover the pass. Disabling the steering hold fails 4 tests, and disabling the holding in the pass fails 10.
Ported from `repro_refute_steerwindow_k2v.rs` and `repro_ctxgen_core.rs`.

Verified: `cargo test -p harness` (403 unit, 16 in `state_bounds`) and `cargo check --workspace --all-targets --locked`. No live
test ran. `target/` was removed first under the package's 30 GiB rule (27 GiB free).

Handoff:

- `docs-and-review-notes`: `how-it-works/context-and-storage.md` near line 108 says entries "follow their recorded positions".
  It should add that a tool group renders adjacent, with anything arriving inside it behind the last result.
- Anyone touching `llm-runtime`: the interleaving case in the `push_block` comment in `anthropic_messages.rs` no longer reaches
  it from the harness. It is harmless, and I left it because this package is harness only.

Independent review of `context-render`: no behavior defect found, and the report holds. Re-ran `cargo test -p harness` (403 unit,
16 in `state_bounds`) and repeated both mutations: 4 and 10 failures, as stated. Also ran the `temporal-workflow` and
`api-projection` unit tests, which the package had not, since the steering hold is visible to hosts. They pass.

Traced and found sound: every request and compaction reads the pass; no request path sends a delta by position; no request goes
out while a group is open, so the rendered prefix cannot move; failure, cancel and force-cancel paths never wait on steering behind
an open batch, so the hold cannot wedge a run; `fail_run_proposals` writes the closing results before steering; provider
compaction prunes by entry id, and a later turn's output flushes anything held, so held entries sit below its compaction entry.

Changed: the `planned_context_entry_ids` comment still said every other entry renders at its entry position.

Left for the checkpoint: `temporal_live_steering_lands_at_next_turn_boundary` covers the hosted window but only checks a steering
entry exists, not its order. The drive tests carry the order. Its neighbor's comment, "materialized while the call is in flight",
was already wrong before this package and is not touched here.

### `api-waiters` (H-24, H-86, H-32)

Landed:

- `ece87096` (H-24, H-86): every command the gateway signals gets a fresh correlation token from `submit_core_command`, and its
  wait reads only that token's failure. The uncorrelated path and all baselines are gone. `admission_failures` keeps 256 through
  one `record_admission_failure`. Status lists `pending_submission_ids`, and run start checks that instead of the queue count, so
  `wait_for_admission_drain` is gone. Close lost its precheck. `ManualCompactionOutcome` replaces
  `last_manual_finished_revision`, and a failed compaction returns `rejected` with its reason. `ApprovalAlreadyTerminal` maps to
  conflict. Steering takes a `submission_id`, deduped at admission against `active_run.steering`; bots pass one derived from the delivery id.
  `OperationPending` (-32015) answers expired waits, with `submissionId` on run start, and `ApprovalDecisionStatus::Pending` is the
  per-decision form. The web retries a held start with the same id.
- `72a7ead6` (H-32): `RunFailureView` on both run views. Start, cancel, steer and decide return `RunSummaryView` and lost their
  budget exemptions. The new `gateway/service/run_events.rs` reads a run in pages, counts only its own events, and sets
  `earlierEventsOmitted`. Tool status, `outputRef`, preview flags, `outputBytes` and `truncated` are in. Media previews survive a
  run's end, input items carry their names, and names are capped at 256 characters. `failure_message_ref` on the status summary
  is deleted. API contract and TypeScript consumers are regenerated; the workflow contract exporter left no diff.

Calls made:

- Cancel and environment activation also take tokens. The brief said cancel needed no change, but it costs nothing and catches
  an internal refusal, and the uncorrelated path could then go entirely.
- Force close also sends a token, and any failed wait still falls back to terminate and reconcile.
- One deadline covers a whole decision batch. After the first expiry the remaining decisions are signalled and report pending at
  once, instead of 64 decisions costing 90 s each.
- `ApprovalDecisionFailureKind::Internal` is new, for a decision whose outcome cannot be read because of a failing workflow, a
  query error or a failed signal, none of which is a refusal. A failed signal no longer discards earlier results.
- A decision only reads `decided` if the record shows what this caller asked for. Otherwise it reads `alreadyDecided` or `cancelled`.
- The steering waiter also compares input, so a reused id with other input waits for its rejection rather than taking the existing
  batch for success. A client that omits the id gets a server-allocated one.
- `submission_id` is not on the public `runSteeringAccepted` event view. Nothing needs it, and adding it can come later.
- `RunFailureView.message` is bounded to 512 bytes on summaries, sharing the input preview's constant, and whole on reads.
- Usage for a run read back from the log is summed while paging, through `harness::accumulate_run_usage`, so the window cannot
  undercount it.
- The CLI appends a failure to a rebuilt turn only if it did not see that failure live, through `observed_failures`, so a live
  session does not print it twice. It also shows a notice for an omitted run and reports no tool count for one.
- The web now sends a submission id with steering too, through an optional `submissionId` on `sessionSteerSchema`.

Deviations:

- No web notice for `earlierEventsOmitted`. The web's only `session/runs/read` consumer converts a queued message to steering, and
  a queued run has no events. The transcript reads the event stream. There is no surface for the notice, so the CLI has it instead.
- The dead `let _ = config;` in the brief was already gone.
- The judge's distinct over-limit error is moot, since long runs are no longer rejected.

Verified: `cargo test -p harness` (407 unit, 16 in `state_bounds`), `cargo test -p temporal-runtime --lib gateway` (208), `cargo test
-p temporal-workflow`, `-p api-projection`, `-p api` and `-p cli`, `cargo check --workspace --all-targets --locked`, `npm run typecheck`,
and the web (1006), backend and TypeScript client tests. Ported from `repro_apiview_zl4`, `repro_second_decision_kind_y2j7`,
`repro_steerdup_refute_n8y2`, `repro_admfail_growth_u3k9` and `repro_runceiling_intervalscan_e5b`.

Narrow live runs, because correlation only exists end to end through Temporal, against `lightspeed_hardening`. All passed: the new
`temporal_live_command_waits_answer_from_their_own_outcome` (plain close refused within 3 s, steering retried once and reused with
other input), steering, cancel mid-generation, queued runs, `temporal_live_anthropic_standalone_compaction_and_continuation`,
`temporal_live_admission_failures_do_not_poison_workflow`, `temporal_live_mcp_approval_approve_and_reject_continue_runs` and
`temporal_live_native_mcp_search_call_and_approval`.

Not exercised: a failed manual compaction and an expired wait through a running stack. Both are covered only by reducer and helper
unit tests, because the gateway's waits need a Temporal client and no fake provider fails a compaction. A close refused for a
detached promise was not run live either; the live test's refusal comes from an active run, and the promise case rests on the reducer
test of the same check.

Open:

- `temporal_live_native_mcp_search_call_and_approval` hung for over 10 minutes when `LIGHTSPEED_MCP_PRIVATE_NETWORKS` was unset, and
  passes in 5 s with it set to `127.0.0.0/8`. I did not trace why it hangs instead of failing, or check an earlier commit. The passing run
  left its Configurator node process behind with no parent, which I stopped by pid; I did not check whether that always happens.

Handoff:

- `format-guards`: `SteeringAccepted.submission_id` and `SteeringBatch.submission_id` are required, `last_manual_outcome` replaces
  `last_manual_finished_revision` in checkpoints, and terminal records now hold media previews.
- `docs-and-review-notes`: `how-it-works/agent-loop-and-durability.md` near line 63 should give the consequence of a held start,
  `operation_pending` and retrying with the id. `integrating-and-extending/api-and-typescript.md` near lines 178 and 267 should cover
  summaries from mutations, the steering `submissionId` and `earlierEventsOmitted`.
- `rename-casing`: `ApprovalDecisionStatus::Pending` and `ApprovalDecisionFailureKind::Internal` are new closed-enum values.
- `bot-delivery-wedges`: see the steering case under Risky calls, if it wants to close it. A bounded delivery attempt count would also
  remove the unattended source of admission failures.

Independent review of this package, starting from its report. It held up: tokens, status, the failure cap, the reducer's dedupe and
refusals, the run read and the projection all do what the briefs say, and the generated contracts were current. Fixed:

- Bot steering sent the raw delivery id. A delivery of one event keeps the event id, and a manual `eventId` takes up to 200 bytes of
  anything, but a submission id is 128 characters of a narrow alphabet. That delivery failed with a non-retryable `invalid_request`,
  a regression from this package. Steering now uses `delivery_submission_id`, as the run path does, so a delivery's run and its
  steering share one id. They live in separate dedupe sets, and neither waiter can take the other for its own.
- The new 256-character media name cap also reached bot events, so a long file name from a channel refused the delivery. The cap is
  now `api::MAX_MEDIA_NAME_CHARS`, documented on the field, and bot rendering cuts a long name. Direct callers are still rejected.
- `AgentApiError.submissionId` says it is present on any call that took a submission id, but only run start set it. Steering sets it
  too, which is what lets a caller that omitted the id retry safely.
- A pending decision read as applied: no feedback in the web, "approved" in the CLI. Both say it has not applied yet.
- The web labelled a compaction wait that ran out as "Compaction failed". It shows no error now, and the session view reports progress.
- The web sent a steering id but never re-sent it, so a user re-sending a held steering landed it twice. It follows a held steering
  through `postUntilAccepted` now. `lib/sessions/run-start.ts` is renamed `held-request.ts`.
- Deleted `run_usage_from_entries`, left with only a test calling it. Corrected two stale comments and test data naming old wait messages.

Left alone: `pending_admissions` stays on the status as a diagnostic, though no gateway code reads it now. The web's decision notice has
no page test, because `SessionsPage.test` mocks out the query client that path writes to. Commits: `2663d4b0`, `f33a8696`,
`ab84d3ff`. Nothing here was run live; the unit tests carry it.

Further handoff to `docs-and-review-notes`: the steering pending error carries `submissionId`, and `InputItem.Media.name` is capped at 256.

### `config-admission` (H-62, H-72, H-93, H-51, H-52)

Landed:

- `b8418b61` (H-52, H-51): timers select `timer()`, sub-agents otherwise select `enabled()`, so a sub-agents-only profile has no sleep tool.
  The comment on `ReplaceSessionConfig` states the idle precondition.
- `51ea75ea` (H-72): `validate_session_document` is the one sequence start and put run. `validate_subagent_deadline_for_existing_bindings`
  stays on put. A setup error seen while waiting for a session to open stamps the creator, and on a server-allocated id also force-closes,
  returning the original error. The force path of close moved to `force_close_session`, so this needs no close permission.
- `f8f3546e` (H-72): live proof through a running stack.
- `135d9d5e`, `c2bcccf9` (H-62): `mcp_inventory` replaces `desired_mcp_tools` and reports what cannot resolve. Preparation leaves those tools
  out, so the usual patch removes them, and carries a notice the workflow publishes in the same step as `runtime.catalog.mcp`, removed when
  everything resolves again. `NewlyNamed` holds the one rule on who can be refused. `attached_resources` no longer lists servers. A dangling
  tool choice goes to FailRun. API and workflow contracts regenerated.
- `7860577d` (H-93): admission works out the pointer after the clear, then fills it. The gateway refuses with `invalid_request` a request that
  changes the default to a machine failing `selectable`. The web activates or deactivates after the put.

Calls made:

- Strict applies to whole attachments, so a changed tool subset on a held server counts as newly named. A typo in a subset is still refused,
  and a disabled server refuses it too.
- A start is strict only when the request passes `features`, because explicit features replace a profile's whole. A start passing only a
  model beside a profile stays profile-derived.
- Inline profiles on `session/start` count as profile-derived, though a caller wrote them.
- What degrades: a refusal of kind `not_found`, `rejected` or `invalid_request` from resolving one attachment. Anything else, such as a store
  outage, still fails preparation so Temporal retries it. Tool name collisions still refuse, since they say nothing about a server going away.
- The notice is carried on `SessionToolsetPreparation` rather than built in the refresh activity, so the same catalog read decides the tools and
  the notice, and a queued run behind an active one gets it with its deferred patch. It is skipped under a pending compaction, because context
  cannot change then, and the next preparation republishes it.
- The notice has no provenance snapshot. Nothing would read one, and the text already holds what it says.
- The default check runs before the existence check, so a missing default reads `invalid_request` and not `not_found`, as instructed.
- Failed setup on a client-supplied id also stamps on the recovery paths, because the settlement sits inside `wait_for_open_session`.

Deviations:

- The brief's "closed default fails at tool use" does not match `prepare_profile`'s existing check. See Risky calls.
- Also added beyond the briefs: the wording of the MCP delete dialog and disabled status, which said only new sessions were affected, and one
  sentence each on `session/start` and `session/config/put` in the method manifest. Those are short because the manifest caps a description at 360 bytes.

Web wording for review: the active environment help now reads "The one universe environment used by process and environment-targeted tools. Selection
does not transfer ownership. Saving a new default environment activates it when nothing else stays active." The MCP delete dialog reads "Sessions and
profiles attaching `<id>` keep working without it: their next run leaves its tools out and tells the model why." The disabled status reads "Disabled. It stays
configured but cannot be newly attached, and sessions attaching it run without its tools until it is enabled."

Verified: `cargo test -p harness` (406 unit, 16 in `state_bounds`), `cargo test -p temporal-workflow`, `cargo test -p api`, `cargo test -p temporal-runtime --lib gateway`
(216), `cargo check --workspace --all-targets --locked`, `npm run typecheck`, and the web tests for the MCP page and session components. Ported from `repro_cfgprep_zq9`:
the swap case, in `a_put_fills_the_pointer_it_leaves_empty_only_with_a_new_default`. Its identical-document test asserted the behaviour H-51 keeps, so it was not ported.

Narrow live runs against `lightspeed_hardening`, because all three depend on the workflow accepting these commands in these states. All passed:
`temporal_live_a_failed_start_leaves_nothing_live`, `temporal_live_unavailable_mcp_server_degrades_runs_and_refuses_only_new_attachments`, `temporal_live_a_put_can_swap_the_default_and_refuses_one_it_cannot_select`,
and the rest of `profiles_live`.

Not exercised: a wake-up run from a settled detached promise on a session with a left-out server, which takes the same preparation path as the plain runs the live test
covers; a real bot ensure; and a provider request whose history holds the left-out server's calls. The live suite uses the fake model, so it shows the notice in session
context and not in a rendered request. `mcp_live` beyond the new test, `subagents_live` and `bots_live` have not run against this package, and belong to the checkpoint.

Handoff:

- `mcp-native-default`: an attachment active in the catalog but unreachable is still not covered by this rule, as the H-62 judge noted. If it wants the same treatment,
  `runtime.catalog.mcp` and `UnavailableMcpServer` are the place. `mcp_inventory` is the renamed `desired_mcp_tools`.
- `bot-delivery-wedges`: run refusals from a disabled server are gone, which removes the source of the unbounded lane retry the H-62 brief describes. A closed default on a bot's
  profile still fails at ensure; see Risky calls.
- `docs-and-review-notes`: `environments/using-environments.md` near line 239 still says a plain put never activates a default. It should say what `EnvironmentAttachment.default` now says.
  The MCP guides should describe the one rule on refusal and the notice, and the sessions guide what a failed start leaves. Check `how-it-works` for a session's own tool choice being
  refused at run admission.

Independent review of this package, starting from its report. No behaviour defect found, and the report holds. I traced the tool and notice publication on both paths
(direct, deferred behind a frozen batch, and the operation candidate), the fill through `configure` and `apply_profile`, every session start caller (bot ensure and
sub-agent spawn both pass `config: None`, so neither can be strict), the settle on setup failure, and the web save order, including a dialog reopening after a fill. Live tests
checked by reading: none pins the old refusal, and every sleep user outside the new sub-agents arm also grants timers. Regenerating every contract and consumer left no diff.

Re-ran `cargo test -p harness` (406 unit, 16 in `state_bounds`), `cargo test -p temporal-workflow`, `cargo test -p api`, `cargo test -p temporal-runtime --lib gateway` (216),
`cargo check --workspace --all-targets --locked` and the two web test files for the MCP dialog and the settings save. Live tests were not re-run; the only code change is a comment.

Changed: one comment that still said everything attached must exist. Added two notes under Risky calls that the package left out: the catalog edits that still refuse at preparation,
and a setup outlasting the wait. The provider-history risk stands unproven, and `mcp-native-default` narrows it, since Native servers leave no provider-side `mcp_tool_use` blocks.

### `mcp-native-default` (H-19, H-66, H-84, H-83)

What landed:

- `b6eb8d71` (H-19): `tools::limits::projected` is the shared helper. `store_native_mcp_result` applies the 64 KiB budget before storing, sets `output_bytes` and `truncated`
  from it, uses the cut text for `error_ref`, and keeps the full output in `output_ref`. Media entries stay attached when their announcement falls past the cut. The comment
  on the 2 MiB bound now calls it a wire bound.
- `392b857b` (H-66): a failed fetch in `list_tools` keeps the last list for that server, revision and tool subset, or an empty one, and answers for 60 seconds through
  `MCP_INVENTORY_FAILURE_TTL`. The 256-tool cap stays a hard error. The cache key includes the attachment's tool subset.
- `8bd6a002` (H-84): `RemoteMcpToolSpec` and `RemoteMcpCallTarget` lost the URL, private-network flag and both auth fields, and `SecretRef` moved to llm-runtime. The native path
  reads the record per fetch and per call through `usable_record`. Provider-hosted requests ask `McpInventoryResolver::server_url` during materialization. `session/read`'s tool
  view lost `serverUrl` and `authRequired`. The contract and TypeScript consumers are regenerated; the workflow contract did not change.
- `55ade8d3` (H-83): Native is the default in the API, the CLI, the web form, the demo route and the domain enums. `RemoteMcpExecution`'s doc states at-least-once.
- `346c029e` (H-66): recoverable store failures in the LLM adapter are `Retryable`, and the retry helper is now `transient_llm_failure`.
- `3dd8c2a5` (H-84): `mcp/servers/put` says a URL is stored and shown as written. A stale broker comment is gone.

Calls made:

- One resolver trait. `server_url` sits on `McpInventoryResolver`, because the adapters already hold it and the alternative was a second parallel wiring path. The name is
  now slightly narrow; `rename-vocabulary` can change it.
- The record owns the auth decision. `BrokerSecretResolver` reads the policy: Required with no grant is an error, None or Optional with no grant is `CredentialAbsent`. Callers
  use `resolve_mcp_bearer` and hold no auth flags. As a result an adapter with no configured secret resolver fails for a no-auth provider-hosted server too. Production always
  configures one, and the two provider live tests now pass `AbsentSecretResolver`.
- `inject_remote_mcp_auth` takes the URL from the materialized entry, so a server left out needs no credential and the record is not read a second time for the URL. The
  resolver does read it again for the credential, and if the URL moved in between it refuses with a message naming no URL.
- A provider-hosted server whose record is disabled or `needsAuthConfig` is left out like a missing one. It used to fail the turn when it had auth, and went out when it had none.
- The H-62 handoff is closed by not adopting it: an attachment that is active in the catalog but unreachable gets no notice, because the owner accepted the silent cold-worker case.
- `McpInventoryError` gained `transient`, used only by `server_url`, whose failures after the not-found case are store reads. `SecretResolveError::Backend` stays terminal, as the
  judge advised, because it also covers permanent conditions.
- The SQL column default stays `provider`, since every put binds the column. A migration for a value nothing reads is not worth it.
- The demo's seed-data helper keeps `execution: "provider"`, since it describes seeded servers rather than new records.

Deviations:

- The store-error classification is not in the package instructions. The H-66 brief says it still applies and no other package carries it, so it landed as a separate commit.
- `ConfiguratorTrustedHeaderPolicy::permits` always returns false, so its branch is dead code in the code I touched. I left it, since removing it touches the gateway and the transport signature.
- `crates/mcp` and the web demo were touched beyond the listed crates, for the comment and the defaults.

Ported as tests beside the code: `repro_mcpbudget_refute_e8r4` as `a_large_native_result_is_cut_for_the_model_and_kept_whole_by_reference`, and
`repro_mcpurl_token_durable_v9r2` as `mcp_attachment_leaves_url_and_credential_out_of_durable_state` and `remote_mcp_durable_shapes_hold_no_transport_facts`. The H-66 prototype's
tests are in `worker::mcp`. New tests also cover a live URL per fetch, a native call against an unusable record, a left-out provider-hosted server on both adapters, the credential
policy, and errors that echo no URL.

Verified: `cargo test -p harness` (405 unit, 16 in `state_bounds`), `cargo test -p llm-runtime` (212), `cargo test -p api` (104), `cargo test -p temporal-workflow`,
`cargo test -p temporal-runtime --lib` (526), the CLI test, `cargo check -p temporal-runtime -p temporal-workflow -p cli --tests`, `cargo check --workspace --all-targets --locked`, `npm run typecheck` and the two
`McpServersPage` web test files. Live, against `lightspeed_hardening`: `temporal_live_native_mcp_search_call_and_approval` passes in 6 s.

Not exercised: the rest of `mcp_live`, `mcp_oauth_live`, `code_capabilities_live` and both provider MCP live suites, whose fixtures changed and only compile. They belong to the checkpoint.
No real provider has received a request built from a live record.

Correction to the note under `config-admission` and earlier about `temporal_live_native_mcp_search_call_and_approval` hanging: the test does not hang. It passes in seconds, and its
Configurator child outlives it holding stdout, so a `cargo test ... | tail` never returns. I reproduced that with the env set to the dev default, then ran it writing to a file. The leak
is real: three such node processes from earlier runs (pids 15877, 15882, 15886) were still alive when I finished. I killed only the two my own runs left. Run the
suite with output to a file, or fix the child's lifetime in `LiveConfigurator`.

Handoff:

- `rename-vocabulary`: `RemoteMcpExecution::Native`, `McpExecution::Native` and `RemoteMcpExecutionArg::Native` become Runtime. `McpInventoryResolver` could become a server resolver, and the
  error type `llm_provider_transient` no longer only means the provider.
- `docs-and-review-notes`: the MCP guides should state that native is the default, the at-least-once rule for provider-hosted tools, that URL and credential changes apply at once to running
  sessions, including an approved call, and that a failing server keeps its last list or none. The session tool view fields are gone from any example showing them.
- `format-guards`: `RemoteMcpToolSpec` and `RemoteMcpCallTarget` changed shape, and `remote_mcp_durable_shapes_hold_no_transport_facts` pins their keys.

Second check of this package against its briefs, by reading the code and re-running tests. All four items are done, and no source changed. I traced the projection on every native outcome, the
keep-last branch and its cache key, the live record on each fetch and call (no revision or URL comparison, so an approved call follows an admin's edit), and every error path a URL
could reach. Transport and `safe_http` errors carry fixed text, and `AudienceMismatch` is rewritten to name the grant. `mcp_find_tools` pages stay within the 64 KiB budget, so
`nextCursor` is never cut off.

Re-ran `cargo test -p harness` (405 unit, 16 in `state_bounds`), `cargo test -p llm-runtime` (212), `cargo test -p api` (104), `cargo test -p temporal-workflow`, the CLI's `mcp_cli` filter,
the `temporal-runtime` lib filters `worker::mcp` (25), `worker::secrets` (15) and `activities::tools` (12), `cargo check -p temporal-runtime -p temporal-workflow -p cli --tests`,
`npm run typecheck` and the `McpServersPage` web tests (15). The SDK and Configurator generators left no diff in generated files. Live tests were not run.

Independent review of this package found three real gaps, which the two checks above missed, and fixed them:

- `9ee4c323` (H-84): `URLs no longer reach model-visible text` was false. `validate_mcp_grant_for_server` formatted the URL and OAuth resource into its refusal, and since `config-admission`
  that refusal becomes the text of the Catalog notice when a grant stops covering an edited record. A token URL was written to session context and shown to the model. It now names the grant only.
- `4498bc4f` (H-84): `server_url` ignored the record's execution. After a flip to native mid-run, a session whose spec still says provider sent the record's URL, possibly private, and its
  credential to the model provider until the next preparation. Such a record is now left out, matching the native path's refusal of the opposite flip.
- `e402cc49` (H-19): the risky note that an argument error "is cut the same way" was false. `failed_tool_call_result` wrote failure text uncut, and an argument error quotes an input schema of up
  to 512 KiB. It now applies the budget for every tool, keeps the identifying trailer, and sets `output_bytes` and `truncated` only when it cut. The note above is now true.
- `a72b4afb` (H-83): the demo route honoured search exposure only for an explicit native, which disagreed with the new default.

Checked and left: replay and determinism are unaffected, since the spec only lost fields. The contract copies match and `cargo test -p api` passes. The live tests that set an execution all set it explicitly, so
the default change does not move them. The no-object-store retry stays as recorded under Risky calls. A typed variant would touch seven match sites, some in files other agents hold, and a PostgreSQL-only
deployment already fails on any blob above the 64 KiB inline limit. This remains the first follow-up if that deployment matters. `ConfiguratorTrustedHeaderPolicy` stays dead code for the same reason of scope.

Checks: `cargo test -p temporal-runtime --lib` with filters `worker::mcp`, `worker::activities::tools`, `worker::secrets`, `grant` and `mcp_`, `cargo test -p llm-runtime --lib` (218), `cargo test -p api`. No live test was run. `mcp_live`,
above all `temporal_live_unavailable_mcp_server_degrades_runs_and_refuses_only_new_attachments`, should prove the first fix at the checkpoint.

### `release-contract` (H-68)

Landed:

- `dbb1fb29`: SIGTERM resolves `shutdown_signal` beside SIGINT, so a Kubernetes stop shuts down gracefully. A signal that cannot be watched now warns and leaves the other in charge. Before,
  a failed SIGINT listener stopped the process. `sigterm_starts_graceful_shutdown` sends the test process a real SIGTERM.
- `7fe043ac`: `previous_bot_session_ids` drops its `created_at_ms` filter, and `start_bot_controller` loses the parameter. A controller that starts with no carry begins past every retained main and
  per-key generation. Per-event ids are skipped.
- `627483cd`: `scripts/release/workflow-compat.sh <from-ref> [to-ref]` prints the diff and then `breaking` or `restart-in-place`.

Calls:

- Per-event ids are excluded twice: in SQL, because a busy bot can retain any number of them, and in `carry_after_previous_sessions`, where a unit test can pin the rule.
- The admission module header called the event row authoritative. It now says a fresh controller does not rescan stored rows, which is what the owner chose by rejecting H-89.
- The script takes an optional `to-ref`, which the brief did not ask for, so past releases can be checked. It compares whole `temporalio` package stanzas, because a version bump leaves the name
  line unchanged and a grep of the diff would miss it. Both verdicts exit 0, and a usage error exits 2.
- `cargo tree` confirms the script's crate list is the full workspace dependency closure of `temporal-workflow`.

Checks: `cargo check -p temporal-runtime -p store-pg --tests`, `cargo test -p temporal-runtime --lib bots` (38), the SIGTERM test and `bash -n scripts/release/*.sh`. The script prints
`restart-in-place` for `3bed1df8` to `v0.5.1` and `breaking` for `v0.4.0` to `v0.5.0`, and shows the Temporal SDK version change for `v0.1.0` to `v0.5.0`. No live test or upgrade was run.

Open:

- `sessions` has no index on `bot_id`, so the seeding read scans the universe's sessions. Since `b0108550` that happens only when a controller is not running, so it is left without a migration.

Live tests that should prove this: `bots_live_controller_without_state_rolls_past_an_open_main_session` in `bots_live` (added by the review below, compile-checked only). Terminating both workflows stands in for a new
`TEMPORAL_NAMESPACE` on the same Postgres. Reusing a bot id through delete and recreate should still pass in the existing bot live suites. An actual SIGTERM stop of a running stack is still unexercised.

Handoff:

- `docs-and-review-notes`: the upgrade doc needs the rule (run `scripts/release/workflow-compat.sh`; breaking means a new namespace on the runtime and connector host), Recreate rollouts, drain first and the
  accepted losses.
- `bot-delivery-wedges`: H-56 asks to skip per-event bases when seeding from a previous incarnation. That is done here, so only the pruning is left.

Independent review of this package found one real problem and fixed it:

- `b0108550`: dropping the `created_at` filter put the whole retained read on the hot path. `wake_bot_controller` read every main and per-key id on each event and sent them as the start argument of signal-with-start, which a
  running controller ignores. The note above called this a cost that predates the package, but before it the filter returned almost nothing, and bot sessions have no deletion schedule, so the read grows for the bot's life. Once the
  carry passes the payload limit, the server would refuse the call and a healthy controller's events would be rejected. The wake now signals first, and reads and starts only on not found.
- `cbf0be6c`: adds the live test this package said was missing, and stops the script's comment calling it conservative, since it misses non-Temporal third-party bumps.

Checked and left: SIGTERM and its test are sound. Upgraded sockets do not hold up axum's graceful shutdown, and no gateway route streams a response. Force close appends to the log when no workflow exists, so a delete does close old
main rows, as its teardown now spans generations 1 to N+1. The id grammar keeps `:e-` out of main and per-key ids, because bot ids and slugs cannot hold a colon. The script's crate list matches `cargo tree`, and its verdicts for
`3bed1df8` to `v0.5.1` and `v0.4.0` to `v0.5.0` reproduce. No generated artifact changed. `stopCommands` gives every dev process 2 seconds before SIGKILL, so `./dev.sh` may now print a cleanup note if workers need longer. An immediate kill is what the
runtime effectively got before, so this was left.

Checks: `cargo check -p temporal-runtime --tests`, `cargo test -p temporal-runtime --lib bots` (42), the SIGTERM test and the script. No live test was run.

### `oversized-input-refusal` (H-71)

What landed:

- `89e4d3e9`: when the provider confirms an overflow and no smaller cut is left, the failure reads `the smallest complete compaction chunk exceeds the context window; item_17 is the largest entry at about
  262144 tokens: replace it by id or start a new session`. Candidates are the chunk's covered entries and the retained tail, not the summary rolled up so far. `ContextEntryKind::is_replaceable` moved out of
  `validate_entry_replacement` so the text and the admission rule cannot disagree. No event or API shape changed.
- `12655d65`, with the helper in `aeb1e280`: `start_run_internal` and `steer_run` call `refuse_oversized_text`, which counts text bytes / 4 from `stat_blob` per entry and answers `invalid_request` over the limit.
  The limit is `input_limit_tokens` when the run's model is the document's, otherwise `model_discovery.input_limit`. A start checks after the known-submission and open checks, so a retry of an accepted run still
  gets its run. A steer judges against the active run's model. Text documents count as text; PDFs and images do not count.
- `6ac7597b`: the `inputLimitTokens` doc says so.

Calls made:

- Rejected attempts needed no change. `llm-provider-errors` had already stopped counting them, so this package only adds the regression `refused_attempts_leave_the_budget_for_files_that_each_fit` (four and eight
  files of 120k, eight of 70k, known and unknown limit).
- Discovery is only read for text of 16k tokens or more, as no reported window is that small. `reported_limit` is a lazy future, so a test can prove it is not awaited.
- A largest entry that cannot be replaced gets `... and cannot be replaced: start a new session`, since `replace it by id` would be refused. This is beyond the agreed wording.
- A failed blob read while naming returns that read error rather than a text with no name. The same reads had just succeeded.

Deviations:

- The compaction code lives in `llm-runtime`, not in the crates the package lists, so its tests ran there.
- `6ac7597b` was built by editing the committed generated files, because the session restart package's uncommitted method was in the regenerated output. Only that sentence differs from what the generators wrote.
- `aeb1e280` carries the helper in `input.rs`, because that package committed the file whole while both were editing it.

Ported: `repro_fu_h71_paths` and `repro_oversizeinput_wholesys_k3p8` as the harness test `an_input_that_never_fits_fails_each_run_until_it_is_replaced_by_id`, which also replays, and
`repro_judge_oversize_culprit_tail_z8c` as `the_failure_can_name_an_entry_the_compaction_keeps`. The prototype's window and naming cases are in `compaction.rs`. Not ported: the old invalid-request path, which classification now
turns into the same overflow, and the prototype's bytes/2 threshold table, whose estimate is gone.

Added, compile-checked only: the live test `temporal_live_text_over_a_declared_window_is_refused_at_the_door` in `runs_live`, covering a start, a steer on a run in flight, and that run completing.

Verified: `cargo check -p temporal-runtime -p temporal-workflow --tests`, `cargo test -p temporal-workflow --lib compaction`, `cargo test -p llm-runtime --lib compaction` (32), `cargo test -p temporal-runtime --lib gateway::service::tests::`
(91), and the harness `an_input_that_never_fits` and `context` filters. No live test ran. The refusal through a real Anthropic listing and a bot delivery ending as `run_failed` are not exercised anywhere, since `GatewayAgentApi` needs Postgres
and Temporal, so the unit tests cover the helper only.

Independent review of this package found one real gap, fixed in `e18485d8`:

- The refusal gave the run start in sub-agent preparation an answer that never changes, and `prepare` treated every start error as an outage. The activity runs under the default retry policy, so a brief over the child's declared window
  retried with no attempt limit while the parent's reply promise waited and the child held a delegation slot. `prepare` now treats it like a refused session start: the reply promise fails with the reason and the child is closed. The
  256 KiB brief cap is about 64k tokens, so this takes a child profile declaring a smaller window. Covered by unit tests against the fake runtime, for both the refusal and an outage on the same call; no live run.

The report otherwise holds. A bot steer refused this way is non-retryable, and both the lane and the sidecar end it as `run_failed`, by reading only. The contract sentence from `6ac7597b` matches an export of the current source and survived the regeneration in
`5168c73d`. `crates/api/contract` is stale at this point for a different sentence, on `force` in session close, which is not this package's.

Handoff:

- `docs-and-review-notes`: the compaction guide should say a known limit refuses a single oversized text entry at start and steering, and that a compaction failure names the largest entry to replace.

### `bot-delivery-wedges` (H-04, H-22, H-56, H-36)

Landed:

- `56df01f6` (H-56): `forget_routed_session` removes the entry of a per-event base. The seeding half landed with `release-contract`, which skips per-event ids in
  `carry_after_previous_sessions` and tests it, so I left it there.
- `aeb1e280` (H-04): `BotStartRunResult` is `Started`, `Retry` or `Refused`, classified in the activity by error kind. `Refused` returns the lane's `Err`, which already finishes `run_failed` with the
  receipt behind the canned reply. `Retry` and an activity that spent its attempts go through `retry_delivery`: the lane is removed and the delivery goes back with `attempts` and `not_before_ms`, both
  carried. A waiting delivery holds later ones to its target and no others. `running_run` is `steerable_run` and takes `Parked`. Sidecar requeues always carry the busy delay. `BotSessionStatus::Failed`
  comes from `execution_failure`, or from `SessionStatus::Error`, after a check for a closed lifecycle. `Missing` is unchanged. `wait_until_session_idle` moved to `lanes.rs`.
- `fff1613b` (H-22): `reconcile_unless_busy` reads main once per pass, with `reconcile_retry_at_ms` gating `wake_ready` and feeding `wake_deadline`. `target_held` holds only main while it is dirty or
  not ready. `mark_session_degraded` leaves main dirty with a backoff up to 15 minutes, and `reconcile_attempts` and `reconcile_rotated` are carried. A config signal or a reset clears them.
  `authority_error` is gone: `activity_error` keeps denied final and retries a store failure. `chat.tsx` says the bot keeps trying. This commit also holds the H-36 runtime half in `sessions.rs`.
- `c5840135` (H-36): the retention editor and copy are off the bot create and setup pages, and `SessionProfileFields` lost `retention`. `delete_after_close_ms: Some(None)` stays, explained in
  `bot_session_start_params`, with a test.

Calls made:

- **Beyond the task text, from the brief's judge notes.** `map_input_blob_store_error` maps `Store` to `internal`, and `read_input_text` makes undecodable bytes `invalid_request` with the file name.
  Without this a storage blip and a poison file are the same error, and one of them must be wrong under `Refused`. A transcription stat or touch failure is now `internal` too.
- `Started` carries `finished`, so a retry that finds its run ended attaches the terminal itself. Dropping the lane during backoff made that window real.
- The start activity has its own 120 s start-to-close. At 60 s it timed out before the gateway's 90 s wait could answer `operation_pending`, so that mapping would never have fired.
- Refused: invalid request, not found, forbidden, unauthenticated, unset model default, audio and response size. Retry: rejected, conflict, environment not ready, bootstrap failed, operation pending.
  Internal goes through the activity's retries first, then counts as one retry. `rejected` also covers a closed session, whose next attempt ends at the status read in one cycle.
- A sidecar on a `Failed` or `Missing` session ends `run_failed`. `Closed` still requeues, because a rotated routed session sends the delivery to its successor. The lane's steer branch only steers on
  `Busy`, so `Failed`, `Closed` and `Missing` reach the idle wait and end there.
- The separate reconcile at execution start is deleted, since it would degrade on a busy main at every rollover. See Risky calls.
- Rotation is limited per failure streak, which the brief did not ask for. Backoff retries made the old rotate-on-every-attempt create a session per retry.
- The `config-admission` handoff on a closed default: it stays a failure at ensure. For main that is degraded with a retry and the real message, and for a routed session `run_failed` with the reply. No
  new check.
- The `api-waiters` steering case stays open, as bots are work in progress. Steering `operation_pending` still fails a sidecar delivery after the activity's attempts.
- A lane's status read, steer and append failures still end the delivery through `?`, without a carried retry. The planned action only covers the start.

Deviations:

- `api` needed no change. The workflow contract exporter left no diff from this package, as these types are not in it.
- `fff1613b` was committed whole while `bots-dead-session` was in those files, so `lanes.rs`, `controller.rs` and `sessions.rs` hold some of its `SessionDead` work. The variant itself is in `types.rs`, committed separately so the branch compiles.

Ported: nothing from `repro_startrace_refusedlane_n6v2`. It pins harness admission, not this package's code, and the state tests here cover the behaviour it was evidence for.

Verified: `cargo check -p temporal-workflow -p temporal-runtime --tests`, `cargo test -p temporal-workflow --lib bots` (46), `cargo test -p temporal-runtime --lib -- bots::sessions gateway::service::input`
(16), and `npm run test --workspace @lightspeed-ai/platform-web`. The loop itself has no unit harness, so the wake and hold rules are tested on the state and the lane paths only by compiling. Nothing ran live.
Live tests that should prove this: `bots_live`, especially a poison attachment ending `run_failed` with a reply, a config edit during a direct run on main with a routed delivery beside it, steering a
parked run, and a degraded bot recovering by itself.

Handoff:

- `docs-and-review-notes`: `using-lightspeed/bots-and-triggers.md` should say a refused delivery ends `run_failed`, a start is retried for about 20 minutes, a degraded bot retries by itself while its
  routed sessions keep working, and bot conversations are kept until deleted whatever the profile sets. Steering reaches a parked run.
- `format-guards`: `BotDelivery` gained `attempts` and `not_before_ms`, the carry gained `reconcile_attempts` and `reconcile_rotated`, `BotStartRunResult` and `BotSessionStatus` changed.

Independent review of this package, starting from its report. The four items do what the briefs say, and the classification, carried retries, wake rules, generation pruning and retention removal held up.
`repro_refute_steerspin_parkfold_t7j` needed no port either, because `steering_parked_run_is_accepted_and_materializes_on_resume` already pins it in the harness. Fixed:

- `ee68c509` (H-22): the poll taken out of the main loop was still in the lane. A delivery to a session running a direct run held a lane in `wait_until_session_idle`, which blocked continue-as-new for that
  whole run, the wedge H-22 set out to remove. The lane now reads the status once and goes back to the queue on busy, spending no start attempt. `wait_until_session_idle` is gone, and an emitting bot publishes
  its directory only when the delivery can proceed.
- `39fea98e` (H-04): a terminal with no lane no longer pulls the log. That removes the `unresolved` risk this package recorded, and the same pull could skip a newer lane's resolve calls. A terminal that lands
  on a lane whose attempt then goes back to the queue still moves the cursor, which needs a run ending inside one status read or start call.
- `4b57ecc8`: removed the close check in front of the loop, which only guarded the deleted reconcile at execution start.

Corrections to the report above: `session_execution_stopped` is also `Refused`, and the `wait_until_session_idle` move is superseded.

Left open, none from this package: a config signal landing during a main reconcile is lost, because `mark_session_ready` clears `config_dirty` and records the new profile id for an ensure that used the old
config. `LaneTerminal.failure_message_ref` is carried and never read, so a failed run's delivery summary says `run ended failed` and not the run's message.

Verified: `cargo test -p temporal-workflow --lib bots` (48), `cargo test -p temporal-runtime --lib -- bots::sessions gateway::service::input worker::bots` (17), and vitest on the bot components and `BotsPage` (37).
Nothing ran live. Besides the list above, `bots_live` should prove that a delivery to a session busy with a direct run starts after that run, and a controller holding such a delivery can continue as new.

### `process-and-filesystem-tools` (H-39, H-21, H-38, H-67)

Landed:

- `fce74613` (H-39): VFS `write_file` and `write_file_ref` keep the existing executable flag, media type stays as it was. `rename` is overridden on both filesystems and moves the entry unchanged. `read_file_versioned` and
  `write_file_with` use the blob ref as the version, and a mismatch or missing file is `FsError::Conflict`. `update_head` retries on the workspace-wide compare-and-set and re-judges the precondition each time, so a commit to another file no longer
  fails a write. Capture keeps its single check.
- `2d0eb8a8` (H-21): `apply_hunks` is a planning pass over an overlay with no writes, then a commit pass. One write per path, carrying the first read's version. `ToolError::PatchPartiallyApplied` lists what landed and what did not. Both `apply_patch`
  descriptions gained "Nothing changes if a hunk does not apply", and the provider catalog fixture is regenerated. Dead `Hunk` path helpers are removed.
- `53a97e60` (H-38, H-67): `run_process` starts in a spawned task under `AbandonedProcessGuard`, which sends `process/terminate` with `Terminate` if the call is dropped, after the start answers. Closed and Transport during the wait return Running with
  the handle, the start's pid, the pages read and a note, which every presentation prints. Any other read error awaits the terminate before returning.
- `5dcfa707` (H-67): a Process-class boundary failure in `resume_call` ends with "the command may still be running; check before re-running it". The `cancellable` comment now says the guarantee needs a live worker.

Calls made:

- A failure with nothing applied yet returns the plain error, not `PatchPartiallyApplied`, so a read-only filesystem or a lone update conflict reads as it always did. A first step's `create_directory` is not counted as applied.
- A move to its own path is a plain update. Without this, the VFS place-then-remove would have deleted the file. `rename` handles that case as well.
- A write is only coalesced while it is live. `Update a`, `Delete a`, `Add a` writes `a` twice, and an update then a delete writes before deleting. Both are correct and rare, so I kept the plan simple.
- A file changed by two hunks reports `M` once. An add and an update of one path report `A` once. Those used to list each hunk. A file added then moved reports `A` at the destination, where it used to report `A` and `M`.
- Planning checks ancestors from the target upward, and only positive evidence refuses. A backend that will not describe an ancestor, for a scope or any other reason, falls through to commit.
- The guard's cleanup is bounded at 30 s and aborts a start that never answers, so a silent socket cannot hold a task and a connection open. A failed terminate is ignored: `tools` has no logging, and nothing could act on it.
- The note goes in `failure` on the Running result and prints as `[note: ...]` after the output, on every presentation, rather than inside each state line. `failure` already exists in the tool's output schema, so that schema did not change.
- The boundary note also appears on a boundary `Cancelled` and on `continue_process`, which is Process class. With `TryCancel` a cancelled result does not prove a live worker, and the wording says "may".

Deviations:

- The real-daemon test is in `crates/environment-daemon/tests/client_round_trip.rs`, since the daemon's dev dependencies include `tools` and not the other way round. It learns the next process id from a yielded call's handle. Run it with
  `cargo test -p environment-daemon --test client_round_trip`.
- Not added: the capture, edit and materialize assertion in `vfs_transfer_live`. That suite's model is scripted per mode, so an edit means reworking its call counts, and I could not run it. The flag is covered by `edits_and_moves_keep_a_files_executable_flag` and
  transfer already carries `file.executable`.
- `codex.rs`'s one-shot description ("terminated on timeout or cancellation") is left as it is, because the guard makes it true for a live worker.

Ported: `repro_patchpartial_s3f9` and `repro_movecommit_wholesys_k2v9` as `engine.rs` tests, with the old failing assertions turned round, including add and move over an existing file staying silent. `repro_patchretry_shape_k7q2` as the corrected resend test. Not ported:
`repro_cancelleftover_handle_refute_b5w9`, which is the harness half only and is replaced by the real-daemon test.

Verified: `cargo test -p vfs -p tools` (346 and 18 passed), `cargo test -p environment-daemon --test client_round_trip` (5 passed), `cargo test -p temporal-workflow --lib tool_batches` (6 passed), `cargo test -p llm-runtime --test builtin_catalog_parity` (3 passed, fixture regenerated),
`cargo check -p temporal-runtime --tests`. The workflow contract exporter left no diff. No live test or clippy was run, as instructed.

Live tests that should prove this: `vfs_transfer_live`, `code_capabilities_live` and `environment_registration_live`, which use `run_process` through the gateway. Nothing live yet covers a Stop during a foreground command: such a test should cancel a run mid `sleep`, then assert the process
ended with `stoppedBy: terminate`, on a registered daemon too, since the socket path differs there. A lost-connection test should kill the gateway mid-wait and continue the returned handle.

Handoff:

- `docs-and-review-notes`: `environments/processes-and-jobs.md` says "Stop run cancels agent work but is not a guarantee" and that a command can outlive a cancelled call. Suggested wording: a command that has not returned a handle is terminated when its call is cancelled, a command already
  yielded keeps running until stopped or the environment closes, and if a worker dies its command may keep running and the failed call says so. `using-lightspeed/workspaces-and-skills.md` can now say that an edit to a shared workspace file fails with a conflict if the file changed since it was read, and commits to other files do not interfere.

Independent review of this package, tracing the plan overlay through moves, deletes and re-adds, the guard through each way a call ends, and the path each process failure reaches the model. All four items do what the briefs say, nothing rejected was built (add and move over a file stay silent), and the catalog fixture and workflow contract are current. Fixed:

- `eea77a50` (H-21): `PatchPartiallyApplied` took `ToolError` to 168 bytes, past the 128 where `clippy::result_large_err` fires on every `ToolResult` function, so the CI gate would have failed. The summary is boxed, which makes it 104.
- `01f5bafd` (H-39): `a_versioned_write_rechecks_after_losing_the_head_race` only showed the retry landing, and would have passed with the version checked once before the loop. A new test has another writer change the file inside the window and asserts a conflict. The `ProcessRequest.timeout_ms` comment also still said a call never kills a running process.
- `5fd6f324` (H-38): the handle's usefulness was only compared, against a scripted failure. A new real-daemon test closes a connection mid-wait, continues the handle on another connection and reads all the output through a clean exit.

Correction to the report above: "media type stays as it was" describes the code, not the file. A text write still clears the file's media type, as it did before, and only a move or `write_file_ref` carries one.

A further gap for the risk list: a `process/start` that fails with `DeadlineExceeded` is not terminated either, though its connection is alive. Tool-path connections set no default deadline, so nothing reaches this today.

### `session-restart` (H-64)

What landed:

- `892b9ffe`: `AgentSessionArgs.restart`. Bootstrap loads the log, skips the identity check and refuses a fresh row. `restart::recover_interrupted_work` queues the recent run terminals, fails
  non-retry-safe pending code tool calls, fails a non-parked active run through `FailRun` unchanged, queues unanswered pushed invocations, then sets `ready`. Rollovers keep the flag, because their
  args carry no declaration either, and only the first execution recovers. `run_terminal_emissions` is now shared with the normal path. `log_seq`'s doc changed, so the workflow contract moved.
- `5168c73d`: `session/restart`, `SessionRestartResponse.restarted`, `AgentApiErrorKind::SessionExecutionStopped` (-32016), `StoppedExecution::{Restart, Report}` on `start_session_internal`. Only
  public `session/start` restarts. An open row with a stopped execution now answers `session_execution_stopped` from `signal_submit_admissions` and from `prepare_session_operation`, which used to give a
  bare not found. This closes the `store-rejections` handoff about "failed to start (bootstrap)". `BotEnsureSessionResult::SessionDead` is treated as degraded. Contract and TypeScript consumers regenerated.
- `f37e8261`: `POST /:id/sessions/:sessionId/restart`, a Restart action on the banner for `configure_session`, a demo stub, and a 409 with `kind` in `withGateway`.
- `37793241`: a failed error-blob write in the LLM and compaction activities is the typed transient failure (`unrecorded_llm_failure`). The answer to the owner names this as part of the card, and no
  other package carried it.
- `5630f7ad`: `crates/temporal-runtime/tests/session_restart_live.rs`.
- `597bec90`: regenerated output for a force-flag description from `reaper-and-retention` that `5168c73d` picked up from the shared tree. `0b254667`: a CLI parse check.

Calls made:

- The flag stays set across rollovers and recovery runs only when `continuation_state` is empty, which avoids a second flag and a continuation change.
- `ready` is set after recovery rather than at bootstrap, so `wait_for_open_session` returning means the interrupted run has already failed. See Risky calls.
- A `Cancelling` run fails rather than being force cancelled, as specified. It reads as failed, not cancelled.
- A code tool completion the reducer rejects during recovery is a warning, not a fatal error, so one bad call cannot stop the session coming back. Such a call has no parent left to dispatch for.
- `session/restart` on a running session is a success with `restarted: false`, and a lost start race is a success too, so a retry is safe.
- The reuse policy is explicit `AllowDuplicate` on the restart path only.
- The error is named `session_execution_stopped`, not the judge's `session_workflow_failed`, because a terminate or a missing execution also lands there. `session_bootstrap_failed` stays for a row that never opened.
- `start_run_failure` maps the new kind to `Refused` and `activity_error` treats it as non-retryable, so a bot delivery ends rather than retrying a dead session.
- Added to the Configurator tools by default, as `session/close` is. It is operator-level and the method is safe to retry.
- `withGateway` maps the kind to 409 with `kind` set, so a client can tell it from other conflicts.

Deviations:

- `clients/typescript/test/client.test.ts` had a stale `session/config/put` description from `config-admission`, fixed in `5168c73d` since the file was already in the commit.
- The earlier `session-liveness-read` handoff about main-loop exits without `record_error` needed no change: every exit in `AgentSessionWorkflow::run` now goes through `fatal`.

Ported: `repro_fu_h64_restart_r3k` (a1, a2, b1, b2, b3, c, d, e1) and `repro_deadwf_restartable_s8j2` as `restart.rs` tests, with `FailRun` in place of the `CancelRun` the repros tried. e2 is not ported:
a bad `tool_choice` now fails at the run's own path through `config-admission`. The idle compaction repro is not ported, because idle compaction no longer happens by itself and recovery leaves a run-less
session alone. New tests cover what counts as parked, which code calls fail, both emission windows and every gateway branch.

Verified: `cargo check -p temporal-workflow -p temporal-runtime -p cli --tests`, `cargo test -p temporal-workflow --lib` (227 passed), `cargo test -p temporal-runtime --lib` (561 passed, 1 ignored), `cargo test -p api`
and its `schema_artifacts`, `cargo test -p cli`, `npm run typecheck`, and the three touched TypeScript test files. No live test or clippy ran, as instructed.

Live tests that should prove this: `session_restart_live` (written, compiled, never run), then `sessions_live`, `runs_live` and `workflow_tool_plugins_live`, since the session loop's first pass changed for restarts only.

Independent review of this package found two small gaps, fixed:

- `3710cb8c`: `stopped_execution_not_found_error` fell through to `session_bootstrap_failed`, or to not found, when the row read itself failed. A brief store outage then told a caller the session had failed to start, or did not exist. The store's error now comes back as itself.
- `d2ee9b68`: the `session/start` text still said a retry only returns the session, and `session/managed/start` did not say it never restarts. Both are on the manifest now, with the contract and consumers regenerated.

The report otherwise holds, by reading plus the checks below. Traced: a parked run has nothing in flight, because a group parks only once its window drains, and a decided approval moves the run to `Active`, so an approved call in flight fails rather than running twice. A lost `pending_tool_batch_resumes` entry re-derives from the satisfied await. A start-on-call execution that finished during the outage comes back through the recovery poll, within `WORKFLOW_EXECUTION_RECOVERY_POLL_MS` of the restart. A pending code call whose completion is refused has no live parent, and `code_tool_request` refuses to dispatch it. No receiver reads `log_seq`, so naming the head for a re-sent invocation cannot move a cursor. In the args a restart leaves empty, `workflow_tools`, the name, metadata and retention only matter to a fresh session, and `setup` only before `ready`.

Two corrections to the report. `5168c73d` also swept in the reaper-and-retention `discard_unopened_session` and `close_best_effort` work, not only the force-flag text, in `workflow.rs` and `subagents.rs`. The emission re-send has unit coverage only: `session_restart_live` does not assert one, so a pass there does not prove it. The toolset, follow-up and compaction facts are added to Risky calls above.

Verified after these commits: `cargo test -p api`, including `schema_artifacts`; `cargo test -p temporal-workflow`, including `workflow_contract`; `cargo test -p temporal-runtime --lib` (563 passed, 1 ignored); `cargo test -p cli`; `cargo check -p temporal-workflow -p temporal-runtime -p cli --tests`; `npm run check:generated`; vitest on `session-lifecycle.test.ts`, `SessionsPage.test.tsx` and `client.test.ts`. No live test or clippy ran.

Handoff:

- `bots-dead-session`: `SessionDead` is typed and returned on ensure. `activity_error` already treats the kind as non-retryable and `start_run_failure` as `Refused`. An operator's `session/restart` on a bot session is the
  one path back; the controller sees it at its next reconcile.
- `reaper-and-retention`: the reaper is deliberately unchanged and nothing restarts on a timer. It may report `session_execution_stopped` if it wants one word for this.
- `rename-wire-identities`, `rename-casing`: the kind and `SessionRestartResponse.restarted` are new wire names.
- `docs-and-review-notes`: `operations.md`, `troubleshooting.md` and `upgrades-and-recovery.md` say a failed session workflow is final and name only force close or a Temporal reset. They should describe `session/restart`, a retried
  `session/start`, the failed run's message, what a restart does not recover, and force close being permanent. The root README and guides are untouched.

### `reaper-and-retention` (H-20, H-90, H-31, H-37, H-23)

Landed:

- `6e93a8cc` (H-20, H-90): the promise reaper lists only open sessions, loads through checkpoints and handles each session in one step inside the list loop, so one state is in memory at a time. Its stores carry no blob cache, so a
  pass cannot evict the workers' entries. A terminal or missing describe is rechecked against a fresh read before anything is logged, and `session_head_seq` and the run id come from that read. Paused and rolled-over executions no longer
  count as stopped, through `ExecutionLiveness::from_status`. Repair logs name the session and promise (`promise_repair_signalled`, `promise_repair_appended`, `promise_repair_repeated`). `SignalledRepairs` holds only the previous pass's
  keys, is rebuilt each pass, and repeats log at warn and count in `holder_repairs_repeated`. The pass summary always logs, with `duration_ms` and `events_read`. `workflow_status_cached`, `ReaperPlan`, `LoadedSessionSnapshot` and
  `SessionLoadError` are gone, and `read_all_session_entries` moved to `eval`. `replay_core_agent_state` had already gone with `dead-surface-removal`, and `CoreAgentDrive::from_replayed` already took its head from the state.
- `ba711995` (H-31): Postgres `create_session` looks for the row after taking the locks and before counting. `discard_unopened_session` removes only a `new` row with no head, on both stores, and is used by
  `force_close_session_in_store`, which `SubagentService::close` reaches through the API. `list_retention_roots_due_for_deletion` takes a `RetentionRootCursor` keyset, and `reap_due_roots` walks every due root in a pass, behind a small `RetentionStore` trait so the walk runs against the
  in-memory store in tests. `open_tree_skips` above zero logs a warning with up to 8 root ids.
- `6cee3be8` (H-37): migration 012 adds `cas_sweep_cursors`, with a foreign key to the universe only. `sweep_universe_from_stored_cursor` reads the position, runs the pass and writes it back once, clearing it at the end of the walk. A dry
  run reads and never writes. `walks_completed` is in the stats, the pass log and the CLI output, and each universe logs `cursor_touched_at_ms` and `wrapped`. `REQUIRED_SCHEMA_REVISION` and `LIGHTSPEED_SCHEMA_REVISION` are both 12, and
  `scripts/release/verify-metadata.sh` passes.
- `78d0dfdd` (H-23): `subagent_activity_options` has the existing 360 s start-to-close and a 15-minute schedule-to-close, with retries from 1 s to 60 s up to that budget. `activity_options()` is unchanged. Activities mark `InvalidRequest`, `NotFound`,
  `Forbidden` and `Rejected` non-retryable, and `read_profile` maps a missing profile to `not_found`. When prepare or resolve finally fails, the workflow closes the child and fails, and the recovery poll fails the holder's promise.
  `subagent_child_session_id` and `subagent_submission_id` moved to `temporal-workflow` so the workflow can derive the child's id. Resolve's close is best effort and logs.

Calls made:

- No in-memory sweep cursor, where the brief said to load one when none is in memory. A process that led, lost the lock and led again would walk back to its old position and overwrite the newer one. Reading one row per universe per hourly pass
  costs nothing, and this removed code.
- The record is read fresh per session rather than taken from the list page. A checkpoint newer than a list-time head is rejected by `load_reduction`, which would log a warning and replay a whole log for an active session.
- `maximum_attempts` is 0 on the sub-agent options, where the judge said bounded attempts. A store refusing connections fails in milliseconds, so a count would run out long before the outage ended, and the schedule-to-close is the real bound.
- An id collision in prepare is now `rejected`, not `conflict`. With a failed prepare now closing the derived child id, an error there would have force-closed a session the execution does not own.
- `Conflict` stays retryable in `subagent_failure`, though `is_caller_error` counts it, because a head conflict on force close heals on retry and the budget bounds it.
- Cancellation of a stuck sub-agent activity is not added. The brief's planned action asks for bounds and not heartbeats, and the token stays detached so a close after a cancel still runs.
- The start-run half of H-23's judge list was already in place from `e18485d8` (H-71).

Ported: `repro_retention_head_block_r9x4` as `blocked_retention_roots_do_not_hide_a_later_due_root_and_are_counted` and `a_rejected_delegation_leaves_the_root_tree_deletable`; `repro_staleprojection_premise_r5w` against the real reaper as
`a_session_that_closed_before_its_describe_is_not_reported_stale`; the stale-head half of `repro_reapercost_v8q3` as the two `a_direct_repair_*` tests; `repro_pgretry_orphan_refute_n4c8` as
`retrying_a_delegated_create_at_capacity_hears_that_the_row_exists`, plus the same assertion in the Postgres live test. Not ported: the cost measurement in `repro_reapercost_v8q3`, which is a benchmark and not a regression test, and
`repro_subagent_parent_ceiling_wait_v4z`, whose tests document the harness's parking rather than these activities.

Verified: `cargo check -p temporal-runtime -p store-pg -p temporal-workflow -p eval --tests`; `cargo test -p store-pg --lib` (14 passed); `scripts/release/verify-metadata.sh`; `cargo test -p harness --lib storage::session` (25 passed);
`cargo test -p temporal-workflow --lib -- config:: subagent` (14 passed); `cargo test -p temporal-runtime --lib -- worker::reaper subagents:: worker::activities::subagents` (40 passed). The workflow contract and public API exporters leave no diff. No live test
or clippy was run, as instructed, so no SQL here has run against Postgres and nothing has run against Temporal.

Live tests that should prove this, after a migrate to revision 12: `store_pg_live` (`pg_live_sweep_cursor_is_stored_per_universe_and_holds_nothing`, and the retry and discard assertions in the origin test), `session_lifecycle_live` for the keyset, and
`subagents_live`, including the new `temporal_live_force_close_discards_a_session_that_never_opened`. The reaper itself has no live coverage, so one pass against a real stack is worth reading in the logs. A live test for a deleted profile ending in a failed
reply would prove the whole H-23 path, and does not exist yet.

Handoff:

- `docs-and-review-notes`: `deployment/operations.md` says `stale_active_projection` means stored state shows active work on a dead workflow, which is now accurate, so it should drop any hint of false alarms. It should add `promise_repair_repeated` as the
  sign of a possibly stalled workflow, with the Temporal UI's pending workflow task as the next step, and say the pass summary always logs. Its retention text should add the warning that names stuck roots and say force close clears a never-opened child.
  `deployment/troubleshooting.md` needs the same. The CAS sweep text should say its position survives restarts, and `walks_completed` should appear with the `cas-sweep` CLI output.

Independent review of this package, starting from its report. The reaper walk, the repeat tracking, the recheck after a stopped describe, the retention keyset, the sweep cursor, the migration and the schema revisions all hold up, and the removals are real. Fixed:

- `97764a24` (H-31): the supervisor's close discarded a never-opened row and returned without stopping the child's execution. Temporal can accept that start with no worker to run it, which is what an outage looks like, so once a worker came back the child's
  `create_or_load_session` created the row again with no origin, and it stayed open as a root nobody closes. The close now always goes through force close, which stops the execution first. The report's risk named only an overrun attempt, so it understated this.
- `5a3b16bd` (H-23): three things. A delegation that finally failed told the parent only that the execution reached a terminal status without resolving, and the execution's own failure text was the activity's top message, `Activity task failed`. The execution now fails
  with the worker's words, and `check_execution` reads them back with `get_result`, for every workflow tool. `model_default_unset` now counts as a refusal, since a profile with no model in a universe with no default otherwise waited out the 15 minutes.
  `subagent_failure` names every kind, and also treats `Unauthenticated`, `AudioBlobTooLarge`, `ResponseTooLarge` and `SessionExecutionStopped` as final. The workflow also closes the child again after answering, which removes the risk that one failed close leaves a finished
  child holding a slot.

Corrections to the report above: "the parent's promise fails through the recovery poll" was true and lost the cause until `5a3b16bd`, and the discard is no longer called directly by the supervisor.

Considered and left alone: a due session updated during the walk can move ahead of the list cursor and wait for the next pass, which only delays a backstop and can undercount a repeat. A prepare failure closes the id derived from the execution, and an unrelated session could
only hold that id by choosing a digest of an execution id that did not exist yet.

Verified: `cargo check -p temporal-runtime -p temporal-workflow --tests`, `cargo test -p temporal-workflow --lib -- subagent config::` (14), `cargo test -p temporal-runtime --lib -- subagents:: worker::activities::subagents worker::activities::workflow_tools worker::reaper` (44), and
`scripts/release/verify-metadata.sh`. The workflow contract exporter leaves no diff. Nothing ran live. Besides the list above, `subagents_live` should prove that a delegation's normal path still ends with the extra close, and `workflow_tool_plugins_live` that a failed
execution's promise carries its failure text.

### `bots-dead-session` (H-69, H-65, H-40)

Landed:

- `6d5cbf5c` (H-40): the close activity forces when `execution_liveness` confirms that session's workflow is not running, per descendant on its own workflow, and never by attempt count. A failed liveness read is not confirmation.
  `CLOSE_WAIT` is 45 s in `wait_for_closed_session`, with a compile-time assert against `BOT_ACTIVITY_START_TO_CLOSE`. `operation_pending` from a close counts as still open.
- `d7bf6e10` (H-65): `notifyOnTerminal` now says delivered to a running controller, others reconcile through `session/runs/read`. `force_close_session_in_store` signals `deliver_emission` for every intent on the retained records after its
  append, and also when it finds the log closed, which is reached only after this close terminated a workflow. `run_terminal_emissions` is exported for this. Corrected: `runs.rs`, the workflow contract text, `PendingEmission`, the flush doc and the reaper
  header. Contract and consumers regenerated.
- `203e4661` (H-69): `roll_forward` in the controller, `retry_on_successor` in the lane, `BotDelivery.rolled_forward`, `BotStartRunResult::SessionDead`, and the `read_dead_sessions` activity behind `sweep_dead_sessions`. `BOT_EVENT_TERMINAL_TIMEOUT` and the "timed out" rung
  of `delivery_outcome` are gone. New live scenario `bots_live_dead_main_session_rolls_forward_with_no_lane_waiting`.

Calls made:

- A delivery that moved carries `rolled_forward` and goes back through the queue, where the brief said the existing retry-once loop. Main needs this anyway, since its successor only exists after reconcile, and a known routed session is only found dead after ensure has returned.
  So one path covers all of them, and a second dead session on the same delivery ends it. The next event starts its own try.
- `SessionDead` from ensuring a routed session returns to the lane with the id that was dead, because a mismatch in the same ensure can already have moved past the lane's target. Closing the lane's target there would have closed the wrong session.
- A failed close leaves the generation where it is and the delivery ends `run_failed`. The next delivery finds the session dead and tries again, so no retry state was needed.
- A per-event base gets a generation entry when it rolls forward, unlike `forget_routed_session`, so the retry has somewhere to go. The successor's close removes the entry, so the carried map stays bounded.
- The reconcile rollover uses `reconcile_rotated`, so a successor that keeps dying costs one generation per failure streak and not one per backoff. `reconcile_unless_busy` treats a dead status on a ready main as the same case, because a closed main answers ensure with `Ready`, and then fails
  on applying the profile, with no typed result.
- Beyond the brief: a lane's append failure reads the status, because the append path has no status read before it and would otherwise fail on a dead session forever. A sidecar keeps ending `run_failed` on `Failed` and `Missing`, without rolling forward, since the sweep owns the session its lane is on.
- The synthetic terminal is `RunStatus::Failed` plus `LaneTerminal.dead_session`, whose text becomes the delivery summary, so the row reads why and not `run ended failed`. `failure_message_ref` stays carried and unread, as `bot-delivery-wedges` recorded.
- `read_dead_sessions` reads at `run_limit: 1`, 8 at a time, and never fails on one session: an unread session counts as alive until the next sweep.
- `roll_session_forward` writes a `last_error` naming the session, so an operator can see why a conversation restarted. For main the next successful reconcile clears it.

Deviations:

- The judge's stale `last_error` note was already settled by `session-liveness-read`, which split out `fatal_error`, and the gateway precheck had gone with `api-waiters`, so no change there. A lane's Busy polling had already become requeue in the `bot-delivery-wedges` review.
- `bots` and `api` needed no code beyond the field doc.

Ported: nothing. `repro_refute_deadlane_parked24h_s9k` pins harness preconditions, not this package's code. `repro_rotate_nonforce_dead_w9c4` is already covered by `close_without_force_still_rejects_active_work` and `force_close_cancels_active_and_queued_work_and_closes`, and its detached-promise
case pinned a precheck that no longer exists.

Verified: `cargo check -p temporal-workflow -p temporal-runtime --tests`, `cargo test -p temporal-workflow --lib bots` (58), `cargo test -p temporal-runtime --lib bots` (46), `cargo test -p temporal-runtime --lib gateway::service::workflow` (12), `cargo test -p api --test schema_artifacts` (9), `npm run check:generated`. The loop has no unit
harness, so the tests cover state transitions and pure decisions, and the lane and sweep control flow are proven only by compiling. Nothing ran live, and no clippy.

Live tests that should prove this: `bots_live`, especially `bots_live_dead_main_session_rolls_forward_with_no_lane_waiting` (written, compiled, never run) and `bots_live_controller_without_state_rolls_past_an_open_main_session`, plus `bots_live_reset_main_recovers_a_missing_session_row` for rotation through the new close, then
`sessions_live` and `subagents_live` for force close and the shorter wait. No live test covers the sweep or a force close releasing a waiting lane, because the fake model finishes at once and the sweep takes 15 minutes. One with a held model and a shortened interval would prove both.

Handoff:

- `docs-and-review-notes`: `integrating-and-extending/workflow-tools.md` should match the new `notifyOnTerminal` wording and say a force close sends the terminal. `using-lightspeed/bots-and-triggers.md` should say a dead session settles `run_failed` and the conversation restarts in a new session without context, a
  run may take as long as it needs, a wedged session holds its conversation until force closed, and rotation forces a session whose workflow stopped. `deployment/operations.md` should add the 45 s close wait and that a force close notifies.
- `format-guards`: `BotDelivery` gained `rolled_forward`, `BotStartRunResult` gained `SessionDead`, and there is a new activity `BotActivities::read_dead_sessions`.

### Independent review of `bots-dead-session`

The items are done as the briefs say, generated artifacts are current (the exporter leaves no diff, `schema_artifacts` and `npm run check:generated` pass), and force close, the sweep and the close escalation read correctly. Three real problems, fixed:

- `84ac8f35`: a retried managed start answers `Ready` for a closed session, and the ensure activity then applied the profile to it, which fails. So a late event for a closed per-event session ended "failed to create session" on every delivery, which the risk entry above claimed had been fixed, and a closed main with the bot not ready and a moved profile degraded forever. Ensure now answers `SessionDead` for a closed session, and the risk entry is now true. The `reconcile_unless_busy` note under this package's calls no longer holds for `Closed`; that branch stays for a ready main whose row is missing.
- `5ef10269`: a parked lane now crosses continue-as-new, as the new risk entry says. `BotControllerCarry` gained `waiting_lanes` and `config_dirty`. The carried dirty flag exists because a dirty main holds its deliveries, so a fresh execution would otherwise have stopped steering into a run on main until the run ended.
- `5ef10269` also: a delivery keeps its token when it rolls forward, and the old session's close reports any run an earlier attempt left there. Depending on where the loop was, that terminal could attach to the delivery's new lane and fail it while its run went on. A terminal now reaches a lane only from that lane's session. A rolled-forward delivery also gets its start attempts back.

One correction to the report: the missing-row risk said the workflow stays up until the bot closes, and that holds only for a main generation.

Left as they are: a closed main with `reconcile_rotated` already spent degrades until a config signal or an operator rotation, which matches the existing once-per-streak limit. A tree with several wedged descendants can spend the close activity's 60 s on 45 s waits, and each retry gets one further, so it converges within its attempts or at the next delivery.

Verified: `cargo check -p temporal-workflow -p temporal-runtime --tests`, `cargo test -p temporal-workflow --lib` and `cargo test -p temporal-runtime --lib bots`. Nothing ran live, and no clippy. Live tests that should prove the fixes, besides the package's list: `bots_live` for the closed-session path. No live test covers a lane crossing continue-as-new. One with a held model, a run across a forced rollover and a terminal in the new execution would prove it, and the same harness could prove the sweep.

Handoff:

- `format-guards`: `BotControllerCarry` gained `waiting_lanes` (new type `WaitingLane`) and `config_dirty`.

### Checkpoint 2

`anthropic_messages_live` (six failures, none in product code, all predating this branch):

- The default live model has been `claude-opus-5-5` since before this branch. It answers 400 to a forced `tool_choice`
  (`any` or `tool`), which failed the tool round trip, tool image and both hosted web tests. It cannot turn thinking
  off, so `none` lowers to adaptive at low effort and the truncation test's `disabled` assertion failed. It answers the
  note that Opus 5's classifier refuses, so the refusal test saw a success.
- Round trips and hosted tools use `auto` with a prompt naming the tool. Truncation pins `claude-sonnet-5`, refusal
  pins `claude-opus-5`. New tests prove forcing on `claude-sonnet-5`, function and hosted, with the wire value checked.
- Call: the adapter does not lower a forced choice to `auto`. A forced choice promises that the turn calls a tool, and
  quietly dropping it would turn that into text with no error. The caller gets the provider's own message as a
  rejection, pinned by `anthropic_messages_live_runtime_reports_a_refused_forced_tool_choice`. No model table was added
  either, since the provider's 400 is exact and a list would go stale. `anthropic_thinking_off` differs because there
  the lowering keeps what `none` asks for.
- Open: `generation.toolChoice` is still offered in the API and the session editor without saying that current Opus and
  Sonnet models refuse the forced forms. `anthropic_messages_caching_live` had the same `RequiredAny` and now uses
  `auto`, compiled but not run in this checkpoint.

`code_workflow_live`, `code_capabilities_live` and `code_model_live` (test expectations, plus one schema fix):

- All 15 `code_workflow_live` failures and both aborts had one cause, and it was the tests. `state-pruning` drops a
  finished invocation and its scope at batch completion, and these suites read `start_requests` and `code_tools.scopes`
  after the run ended. The aborts were a failed count assert and a map index, hidden by `failed to initiate panic`.
  The pruning is intended, so the suites now read `StartRequested`, `ScopeOpened` and `CallAdmitted` from the log through
  `tests/support/code_log.rs`. No check was dropped. The replay check compares admitted calls in the log, and the retry
  test counts `ScopeOpened` and also asserts the scope left state, the first proof of scope pruning through Temporal.
- With the abort gone, `real_model_selects_code_media_and_file_outputs_and_reads_the_presented_image` failed on two
  `code_execute` calls. The model had sent `timeout_ms: 600000`, the schema's `maximum`, and was refused against the
  session's 60000 before retrying correctly. This predates the branch. The schema is built without session config, so
  it no longer states a number. The test stays strict, and passed all five runs after the change.

### `api-reference-corrections` (H-41)

Landed:

- `10232c8b` rewrites the planner order and the emission body doc to the current invariant, and makes `cancel_execution` match `WorkflowInteractionError::NotFound` rather than the text "not found". A finished execution still counts as a no-op. `sessions_live`'s "upserts in place" comment is corrected.
- `f3c97c4a` adds `deny_unknown_fields` to `SessionListParams`, with a dispatch test that `createdBy` and `visibility` are invalid params and every declared filter still decodes, and corrects the `ContextEntryView` prefix.
- `7bc47908` regenerates the contract and consumers.
- Swept into other packages' commits, which took the same files whole while this one was editing them: `15fd653a` holds the manifest strings in `rpc.rs`, `runs.rs`, the `SessionListParams` attributes and the append key doc in `sessions.rs`, the comments in `io.rs` and `command.rs`, and the harness tests. `4083ce6a` holds the `run.rs` and `workflow_tool.rs` comments and the `fleet messaging` removal.

New harness tests beside the code, ported from the review's scratch tests: a keyed non-catalog upsert replaces the earlier entry and goes last under a new id (the old test's name said "in place"), a catalog changing kind replaces every version, a superseded catalog version in the retained tail survives compaction, and cancelling a queued run leaves a session-level compaction running. Steering on an approval-parked run and cancelling a compaction's owning run were already covered, so those two were not ported.

Already fixed by earlier packages, so skipped: the rollover patch, the `Cleared on reopen` comment, `legacy` in `session_state.rs` and `types.rs`, the `original shape` wording, and the `active_run_for_command` comment in `admit.rs`. The run start "context keys" and run read "paged by event sequence" strings were already gone too.

Reference wording for sign-off:

- `session/start` title: "Start a session". `session/managed/start` title: "Start a managed session". Both said "Create or reopen". A closed session cannot reopen, and a retry returns the id or restarts a stopped execution, which the descriptions already say.
- `session/list`: "Returns a cursor-paginated summary list ordered by most recent update, optionally narrowed by filters such as visibleTo, which keeps sessions whose root is shared with the universe or was created by that actor. An unknown parameter is refused. Pages may shift while sessions are changing."
- `SessionListParams`: "Filters for `session/list`. An unknown field is refused, so a misspelled filter fails rather than returning a wider list."
- `session/context/append`: "Admits a batch of keyed context entries with per-entry results. The same content under a key is a no-op. Different content replaces the earlier entry, which leaves active context, and goes last under a new id; a catalog supersedes instead. Invalid input can fail one entry without discarding successful entries."
- `ContextAppendEntry.key`: "Stable client-chosen context key, which names one entry. Re-sending the key with the same content is a no-op, so a retry lands once. With different content the earlier entry leaves active context and the new one goes last under a new id; a catalog keeps the earlier version rendered and supersedes it instead."
- `session/runs/list`: "Returns a newest-first keyset page of bounded run summaries, from current state for recent runs and from the session's log for older ones." Not in the brief; `state-pruning` made the old "current reducer state" text false.
- `session/runs/steer`: adds "A parked run holds it without waking."
- `RunSteerParams.runId`: "The run to steer. Must be the session's current active run, running or parked; a queued, finished or cancelling run is rejected so a late steer never lands on the next run." `items` now says "or wake a parked run".
- `ContextEntryView`: names the `instructions.100.prompts` key prefix.

Calls made:

- The method and types keep the name `session/context/append`, as the judge recommended. "Put" would misdescribe catalogs.
- `deny_unknown_fields` is on `SessionListParams` only, as the item says.
- The test for the cancel match was left out. It is a one-line match on the client's typed error and a handle cannot be built without a client.

Deviation: `7bc47908` was exported while `rename-casing`'s DTO edits were uncommitted in the tree, so it also carries those wire names ahead of their commits. Its regeneration overwrites these files, so the end state is correct, but the commit is not only this package's wording. Reference text in it can be read from the list above.

Checks, with `rename-casing` and `rename-vocabulary` mid-edit in the same tree: `cargo run -p api --bin export-schema` wrote the contract. `cargo test -p api` passes the new dispatch test and `schema_artifacts`, and its one failure, `dispatch_json_rpc_routes_session_close`, is that package's `lifecycleStatus` rename in `tests.rs`. `cargo test -p harness --lib` passed the four new tests by name. `npm run check:generated` fails on that package's uncommitted regeneration, for example `status` to `lifecycleStatus` in `src/generated`, so it does not yet show this package's output is current. `npm run typecheck` reports only error kind and vocabulary mismatches in Platform code, and none on `session/list`. `npm run test:consumers` has 8 backend failures on the same mismatches. `runtime-client.test.ts`'s `session/list` case and `configurator-mcp`'s `http.test.ts` pass, and `gateway.ts` sends only declared fields. All three npm checks need a re-run once those packages commit.

No live test is needed for the wording. `sessions_live` proves the list filter still works through the gateway and the cancel path through `workflow_tool_plugins_live`, where a cancelled execution that already ended must stay a no-op.

Handoff:

- `docs-and-review-notes`: three pages hold stale wording this package did not edit.
  - `how-it-works/agent-loop-and-durability.md`, "Standalone compaction temporarily holds admissions until the compaction operation finishes". Proposed: "Standalone compaction holds admissions until it finishes, except cancellation and force-cancellation. Cancelling the run that owns the compaction abandons it; a session-level compaction continues."
  - `how-it-works/context-and-storage.md`, "Active context retains at most five superseded versions per catalog; compaction clears them". Proposed: "Active context retains at most five superseded versions per catalog. They follow conversation retention, so compaction removes only those inside the prefix it covers." The sentence after it, on ordinary keyed entries replacing their earlier version, could add that the new entry goes last under a new id.
  - `development/changing-contracts.md`, "The current session workflow uses Temporal patching at its active-run rollover boundary". The patch is gone and this branch has no `patched()` markers, so that sentence should go.
- The root README and `crates/harness/README.md` are untouched, as the item says.

Independent review of this package:

- The report above said the reopen clears and the reopen section of `session_lifecycle_live` were already gone. They were not: the dead-code package removed the clears, and checkpoint 1 restored them because that section still reopened a session by appending raw events. The item says to remove the handling, since admission and the reducer refuse a second open. The follow-up commit removes the clears and that section, and the rest of the test now counts from the first close, so its deadline is 45 rather than 65. A raw reopen in Postgres now fails the closed-projection constraint. `session_lifecycle_live` should prove this, since it is the only suite that runs that code.
- `cancelling_a_queued_run_leaves_a_session_level_compaction_running` now asserts the queued run left the queue. It would also have passed on a cancel that did nothing.
- `session/managed/start` in the wording above is now `session/declared/start`, titled "Start a declared session", after a later rename.
- `cargo check -p temporal-runtime` and `cargo check -p store-pg --tests` now pass, so the typed cancel match and the shortened live test both compile. The match was also read against the `temporalio-client` source: `request_cancel_workflow_execution` errors go through `from_status`, which maps `Code::NotFound` to `NotFound`. Other packages were still regenerating when this ran, so the batch checkpoint has to show the contract is current, through `committed_schema_artifacts_are_current` and `npm run check:generated`.

### `rename-casing` (H-82)

Landed:

- `398e0c81` stores bot outcomes and trigger disabled reasons in camelCase: migration 013 re-adds both CHECK constraints with no data rewrite, the store parsers follow, and unit tests tie each stored value to the serde spelling and the migration text.
- `25b5be9d` converts every snake_case enum in `crates/api` to camelCase, including error kinds, `RunFailureKindView`, `UniverseAction` and `ResourceRef`. It adds closed enums for `TurnGenerationCompleted.status`, `ContextCompactionRequested.trigger`, `ContextCompactionFinished.status`, `ContextEntriesRemoved.reason` and `PromiseCreated.source`, and for the four `ContextCompactionView` fields, and `context_compaction_to_api` now branches on them. `contextCompactionQueued { baseRevision }` replaces `manualQueued`, with its own case in `transcript.ts`. `content_ref` and `media_type` become `blob_ref` and `mime` on `ToolAttachmentView`, `ContentRefView` and `RunSummarySourceView::Input`, and only file attachments project onto `attachments`. The contract, TypeScript consumers, demo stubs and web comparisons are regenerated or updated with it.
- `ffdeda61` and `49f863ce` finish the Rust readers of those fields, which were hidden behind other crates failing to build. `63848848` corrects the attachment docs.

Checks: `cargo check --workspace --all-targets --locked`, `cargo test -p api -p api-projection`, `cargo test -p store-pg --lib`, `scripts/release/verify-metadata.sh`, `npm run typecheck`, and the web, connector, backend and client tests covering the changed values. `closed_values_in_the_public_schema_are_camel_case` walks the public schema and holds the casing rule. Nothing ran live, and no clippy.

Live tests that should prove this: `store-pg` `migrations_live` and `bots_pg` for the new checks, `bots_live` for `runFailed` and the disabled reasons through Postgres, `code_workflow_live` and `subagents_live` for media and file attachments, `sessions_live` for the compaction enums, and `authorization_live` for error kinds. No live test observes `contextCompactionQueued`; only unit tests do.

Calls made:

- `ToolAttachmentKind` and `ToolAttachmentView.kind` are deleted rather than renamed. With files only, the field could hold one value. `name` became required for the same reason, since a file always has one.
- The Platform's own values follow the rule too: the web's `manage_access` is `manageAccess`, the connector metric outcome is `rateLimited`, and the SDK's derived `method_not_found` is `methodNotFound`. `SessionInstructionState.active[].contentRef` on the Platform route is `blobRef`.
- Left alone: `mediaHandle`, which the agreed spec does not rename although tool views call it `handle`; the web's internal `TranscriptEntry.contentRef` and `outputContentRef`, which are not wire fields; `SecretRef.namespace` `mcp_server`, which is a stored secret namespace and not an API enum; the workflow contract's snake_case values (`run_terminal`, `limit_exceeded` on `CodeToolRejectionKind`), since they are outside `crates/api`; and `crates/tools/src/content.rs`, as instructed.
- The enums are not named `...View`, matching `ToolItemStatus` and `CodeToolProgressPhase`. The harness has types of the same names, so the projection spells them `api::`.

Deviations:

- The brief says migration 014. `rename-vocabulary` took 014 alongside this one, so this is 013, and `REQUIRED_SCHEMA_REVISION` and `LIGHTSPEED_SCHEMA_REVISION` are 14.
- `ContextStateReplaced` was already deleted, as the package said.

Open: `a_workflow_tool_definition_keeps_its_namespace_apart_from_the_registry_id`, an uncommitted test from another package, used a fixture its type refused. I corrected the fixture in the working tree and left the commit to that package.

Handoff:

- `docs-and-review-notes`: these still use the old values and wait for that package: `deployment/troubleshooting.md` (lines 112, 134), `environments/processes-and-jobs.md` (299), `environments/power-and-cleanup.md` (119), `using-lightspeed/bots-and-triggers.md` (209), `using-lightspeed/models-and-credentials.md` (126, 201), `using-lightspeed/cli.md` (265), `integrating-and-extending/api-and-typescript.md` (239, reading `mediaType` from an output), `platform/README.md` (174) and `platform/connectors/README.md` (32, 33). The upgrade doc should say the casing changed and the database needs purging.
- `format-guards`: stored bot values changed, and no durable harness event did. Workflow state did: `BotControllerCarry.recent_deliveries` and the `BotRecordOutcomesRequest` activity input hold `BotEventOutcome`, so an old bot history reads `run_failed` and fails to decode. The branch purge covers it.

Independent review of this package:

- The landed list above is wrong about where the code went, because agents committed by path in a shared tree. The closed enums in `sessions.rs`, the queued event and the whole `api-projection` change landed in `15fd653a` (filed under H-25). The compaction view enums and the attachment changes in `views.rs` landed in `5adc9885` (also H-25). The old strings left the committed schema in `7bc47908` (H-41). Reverting this item means picking those hunks by hand, not reverting `25b5be9d`.
- `23f55518` fixes a regression people could see. The bot activity chip and the roster line made an outcome readable with `replaceAll("_", " ")`, so a failed event read "runFailed". No test read the text, and the roster test now does. The same call in the status badge was dead and is gone.
- `9edffd7b` corrects the last old value in a public description, `invalid_request` on `deployment/environment-providers/put`. A scan of every description in the schema and manifest found no other. It also carries two wording edits from another package's uncommitted DTO comments in `openrpc.json`.
- `64bdc744` fixes a break from another package, found because the report had only sampled the client suite. `parseEmissionEnvelope` in `clients/typescript/src/workflow.ts` still required `tool_id` after the rename to `workflow_tool_id`, so it threw on every real invocation.
- Also left alone, and not in the list above: `AttachmentSourceView.kind` is an open string whose value `vfs_workspace` the tools write, and which the web matches. `BotEventDocument` summaries for receipts read "finished runFailed" in the model's and the operator's text. Neither misleads, and neither is a closed API enum.
- Read and found correct: every closed enum's spelling against the string it replaced, the compaction view branches against the old string logic, the migration's constraint names against `008_bots.sql`, the absence of any SQL or Rust comparison on the old stored values, `media` on a run read coming from the durable entry events and so surviving compaction, and the risky entries above. A repo-wide scan for the snake_case form of every camelCase schema value found only tool failure codes, separate storage enums with their own typed mapping, and the workflow contract's values.
- Checks: full vitest suites for `platform/web` (1011), `platform/backend` (358), `platform/connectors` (63), `clients/typescript` (15) and `configurator-mcp` (20) pass, as do `npm run typecheck`, `npm run check:generated` (exit 0, before `9edffd7b`, and `schema_artifacts` after it), `verify-metadata.sh`, `cargo test -p api -p api-projection -p bots -p cli`, `cargo test -p store-pg --lib bots` and `cargo check --workspace --all-targets --locked`, with other packages' edits in the tree. Nothing ran live, so the live suites listed above still have to prove this.

### `rename-vocabulary` (H-91)

Landed:

- `fd0170f7` reserves emission for the envelope. `WorkflowToolEvent::Emitted` is `Invoked`, `WorkflowToolEmitted` is `WorkflowToolInvoked`, `emissions` is `bound_invocations`, and the kinds are
  `lightspeed.core.workflow_tool.invoked` and `lightspeed.core.workflow_tool.invoke`, changed through the constants. `WorkflowToolDefinition.revision` is `schema_revision` and `WorkflowStartRef.revision` is
  `recipe_revision`, on their API inputs too. Port residue is gone.
- `73f21af0` makes execution `Provider` or `Runtime` on the harness, registry, API and CLI, with `Runtime` the default. The continuations are `ProviderMcp` and `RuntimeMcp`, with `batch_id` on `RuntimeMcp`. Adds
  migration 014 and `provider_mcp_approval_response` in `temporal-workflow`, used by the workflow auto-reject and the gateway decide.
- `7db653cc` gives managed one meaning. The flag is `lifecycle_controlled` on the record, list filter, views and column, and the creation family is declared, as listed in the package brief. Sessions with no
  declaration are undeclared. Also the notify cleanup and `CodeToolCallOutcomeStatus`. Contracts and TypeScript consumers are regenerated here.
- `d0035b94` holds edits the two slices above left out of their file selection, among them the module declaration for the helper and the wire enum's `Runtime` variant. **`73f21af0` and `7db653cc` do not compile
  on their own without it**, so revert these three together.
- `a0b56345` pins the execution values against the wire and the migration's check.
- `9b61fc53` moves the demo router test from the removed `status` to `activity`, an `H-25` leftover found in the final check.
- `2d2521ed`, from the review of this package, fixes what the rename left wrong. The script had turned "provider-native MCP block" into "provider-runtime" in the public API docs, now provider-hosted. A provider compatibility refusal in `harness` still told operators to switch to "native execution", and adapter locals, comments, test names and `mcp_live` data still said native or emission in the old sense. Contract regeneration before that commit changed nothing else, so the committed artifacts were current.

Calls made:

- Migration 014, not 013. `rename-casing` took 013 first, so the revision is 14 in `REQUIRED_SCHEMA_REVISION` and `LIGHTSPEED_SCHEMA_REVISION`. One migration holds both the MCP checks and the column rename.
- The helper returns the bytes and an entry whose ref is `BlobRef::from_bytes` of them, so callers only store the bytes. I did not add a test that a store's ref equals that digest, and rely on content addressing.
  The third builder, in `openai_responses.rs`, stays: only non-JSON media reach it and `llm-runtime` cannot depend on `temporal-workflow`.
- `admit_bound_notify` and `bound_notify` had already gone by the time this package ran, so only the `admit_notify` test helper and the wording were left.
- `CodeToolCallOutcomeStatus` over a shorter name, because the enum is the status of `CodeToolCallOutcome`.
- The creation fingerprint golden changed because its domain did. The binding fingerprint golden did not change, which confirms the field encoding held.
- No repro test ported. `repro_managedword_twomeanings_g5v.rs` checked the two meanings, and the existing `tool_only` assertions in `harness/src/storage/session.rs` already cover a tool-only declaration reading false.

Deliberately not renamed: `McpInventoryResolver` and `llm_provider_transient` (both "could" in the `mcp-native-default` handoff, and neither misleads badly), the error text "requires batch-unit execution" (`mcp_live` asserts
it), `pending_emissions` and `duplicate_emissions` (these are envelopes), `MessageStatus::Emitted` in channels, and "native" in the filesystem tools, where it means a backend's own search.

Deviation: slices were committed by matching changed lines, and with other agents' edits in the same files, so some of these commits carry a little of their work, and theirs a little of mine.

Checks, with all three renames committed: `cargo check --workspace --all-targets --locked` finished clean. `cargo test -p harness -p api -p api-projection`, `cargo test -p store-pg --lib`, `cargo test -p temporal-workflow --lib`,
`scripts/release/verify-metadata.sh` and `npm run typecheck` pass, as do the touched web, backend and Configurator vitest files. No clippy, full workspace test or live suite ran. The SDK `dist` is ignored by git and has
to be rebuilt before a typecheck after a regeneration, or the backend sees the old method names.

Live tests that should prove this: `mcp_live` for the runtime path and approvals, `workflow_tool_plugins_live` for declared starts and `WorkflowToolInvoked`, `session_restart_live` for the declared path,
`openai_responses_mcp_live` for the provider approval response through the shared helper, `store_pg_live`, `migrations_live`, `session_lifecycle_live` and `audience_live` for migration 014, and the bot suites.

Handoff:

- `format-guards`: changed durable shapes are the `invoked` event, `bound_invocations`, `declaration_version`, `declared_creation_fingerprint`, the `dsc:` prefix, `provider_mcp` and `runtime_mcp` continuation kinds, `runtime`
  execution, `schema_revision` and `recipe_revision`, and the `ControlledSession` type in the bot carry.
- `docs-and-review-notes`: `integrating-and-extending/workflow-tools.md` (`session/managed/start`, `workflowToolEmitted`, the stale `read_tool_emissions`), `integrating-and-extending/configurator-mcp.md`,
  `using-lightspeed/sessions-and-runs.md`, `platform/configurator-mcp/README.md` and any guide using `execution: native` or the `managed` filter. The product term "Native MCP" in the user guides becomes runtime MCP. A grep at review time finds that term in `deployment/configuration.md`, `integrating-and-extending/custom-tools-and-model-providers.md`, `reference/environment-variables.md`, `using-lightspeed/models-and-credentials.md` and `using-lightspeed/tools-and-mcp.md`. The root
  README needs no change for this: its "managed agents" is the general industry term and it names none of the renamed identifiers.

### `rename-wire-identities` (H-25)

Landed:

- `4083ce6a`: `WakeReason` is gone and `await_wake` and the unpark claim carry `AwaitOutcome`. `ContextItemId` is `ContextEntryId` with no alias, the cursor is `last_context_entry_id`, `RunQueueState` is `RunsState`, `recovered_turn_id` is `recovery_attempted_turn_id`. Port leftovers in tool-result proposal building say workflow tool invocation, and the stale fleet mention is removed.
- `15fd653a`: park and unpark for the run-level batch state, with the brief's list: events, codec kinds `batch_parked` and `batch_unparked`, API `toolBatchParked` and `toolBatchUnparked`, `UnparkToolBatch` and its command, `ToolBatchUnparkOutput`, `pending_tool_batch_unparks`, `ToolBatchOutcome::Parked`, the error texts, and "not supported" for fire-and-forget start targets. The six drive `resume_*` methods, `RunStatus::Parked`, `ToolBatchSuspension`, code-tool `Waiting` and `CallDeferred`, and `defer_loading` stay.
- `5adc9885`: `finished_at_ms` on runs and tool calls and `finishedAtMs` on the wire, `runs.finished`, `RunStateRef::Finished`, `RunStatus::Running`, `Promise.deadline_at_ms` and its effect key, `run_timeout_ms` on the sub-agent limits and `runTimeoutMs` on the wire with no behaviour change, `ParkedBatchView` for the host struct, and a refusal that says the run is cancelling.
- `0cee4709`, `e574edb4`: `SessionStatus`, `api_projection::session_status`, `NotLoaded` and the never-sent `session/status/changed` are gone. Session and mutation views carry `lifecycleStatus` and `activity`, and `executionFailure` is the only death signal, so a dead session keeps its stored activity. The bot mapping checks lifecycle, then failure, then activity, with `Missing` unchanged. `ChatSessionLifecycle` loses `NotLoaded` and derives `Error` from `execution_failure`.
- `3994447a`: `workflowToolId` on declarations, `workflowToolsConfigured.workflowToolIds`, `systemWorkflowToolConfigured`, `workflowToolInvoked`, `workflowToolStartRequested`, the harness definition and invocation, `BotToolInvocationRef` and the invoke effect key. `toolId` keeps the registry meaning on `ToolView`, `ToolChoice.Specific`, `ToolCallView` and `ToolCallEventView`.
- `6f968333`: a `ToolId` newtype on `ToolSpec.id` (was `name`), the toolset keys, `ObservedToolCall`, `ToolInvocationRequest`, `ToolPatch`, `ToolChoice::Specific`, code-tool bindings, `sibling_tool_ids`, code-mode `allowed_tools` and `binding_for_tool_id`. `ToolName` now only holds wire names. `tool_catalog.tool_choice` returns its own `LoweredToolChoice { Specific { wire_name } }`, so a harness choice no longer gets rewritten in place. Parameters on `call_execution_spec`, `call_parallelism` and `planned_tool_for_turn` say tool id.
- `b8a9315b`: regressions ported from the repro files beside the code: a parked batch takes results only through the unpark command, steering a cancelling run is refused for that reason, a choice naming a workflow tool id is refused, a declaration refuses the old field, the three lifecycle states and the CLI's lifecycle table. The bot mapping table, including a run waiting on an approval, landed in `ffdeda61` (see the review below).

The workflow contract, public contract and TypeScript consumers were regenerated with the slices that changed them. Other agents' regenerations landed in the same files, since all three renames ran at once.

Verified: `cargo check --workspace --all-targets --locked`; `cargo test -p harness -p api -p api-projection -p temporal-workflow -p cli -p tools`; `cargo test -p llm-runtime -p test-support -p channels -p bots -p eval --lib --bins`; `cargo test -p temporal-runtime --lib`; `npm run typecheck`; vitest for `platform/web` (1011 passed) and `gateway-compaction.test.ts`. No live test or clippy ran. `npm run check:generated` could not be judged at package end because another agent was mid-edit on doc comments in the same DTOs.

Calls and deviations:

- The brief's split of `high_watermark_compaction_proposal` needed nothing: `compaction-triggers` already left it holding only the watermark. `pending_emissions_for_tool` no longer exists: `state-pruning` replaced it with `in_flight_invocation_count`, so only the local proposal counter became `pending_workflow_tool_invocations`.
- `ToolCallStatus::Observed` and `RunFailureKind::Cancelled` were already deleted by earlier packages, so nothing more was done there, and the provider-cancelled shape stays as H-92 left it.
- The effect constant is `EFFECT_WORKFLOW_TOOL_ID` with value `workflow_tool_id`, a little longer than the brief's `EFFECT_TOOL_ID`, because `tool_id` is now the registry meaning.
- Auth flow `completed_at_ms` keeps its name and its column: it is the OAuth flow's own success field and not a run. `session/read` in the CLI's text output now prints `lifecycleStatus`, `activity` and `executionFailure` in place of `status`.
- `CodeExecutionContextV1.allowed_tools` and `OpenCodeToolScopeRequest.allowed_tools` moved to `ToolId` as they hold logical tool identities, which changes the workflow schema with no wire change for a value.
- Temporal live test helpers renamed to `wait_for_session_lifecycle`. An old assertion for `idle` became `lifecycleStatus: open`, and one for `error` became `open` beside the existing `executionFailure` assertion.

Live tests that should prove this: `sessions_live`, `session_restart_live`, `tenancy_live` and `bots_live` for lifecycle and dead-session reads; `runs_live`, `code_tools_live` and `subagents_live` for parking, unparking and the `run_timeout_ms` deadline; `workflow_tool_plugins_live` and `mcp_live` for `workflow_tool_id`, `ToolId` and tool choice; each `llm-runtime` provider live suite for a specific tool choice through `LoweredToolChoice`. They compile against these names but none has run yet. They need a purged `lightspeed_hardening` database and Temporal namespace, since stored event kinds and state shapes changed.

Handoff:

- `format-guards`: changed durable shapes are `lightspeed.core.tool.batch_parked` and `batch_unparked`, `ToolSpec.id`, `workflow_tool_id` on definitions and invocations, `deadline_at_ms`, `finished_at_ms`, `runs.finished`, `last_context_entry_id`, `recovery_attempted_turn_id`, `RunStatus` value `running`, `run_timeout_ms`, and `claim` now serialized from `AwaitOutcome` with the same values.
- `docs-and-review-notes`: guides still using `status` on a session view, `notLoaded`, `toolBatchDeferred`, `toolBatchResumed`, `deadlineMs`, `completedAtMs` or a declaration's `toolId` should move to the new names. The grep of `docs` at package end found none of the first five outside the roadmap; `workflow-tools.md` is the place to check for `toolId`.

### Independent review of `rename-wire-identities`

The brief's items are done. A search of the tree finds none of the old names outside the OAuth flow's own `completed_at_ms`, the bot mapping and refusal read correctly, and `cargo test -p api --test schema_artifacts`, the workflow contract test and `npm run check:generated` all pass now that the tree is quiet, which answers the check the report could not judge.

Fixed:

- `447d51c3`: the hand-written `requireRunStatus` in `clients/typescript/src/workflow.ts` still accepted `active` and refused `running`, and listed a `cancelling_grace` the harness does not have. A terminal emission only carries a finished status, so nothing failed, but it disagreed with the contract. A test now pins the list. The package report ran no SDK suite, which is also how the invocation parser fix in `64bdc744` got missed at first.
- `1b70dfd8`: the invoke effect's `completion_deadline_ms` holds an absolute time, the exact confusion this item split `deadline_ms` over. It is `completion_deadline_at_ms` now.
- `e63683f1`: the workflow status query still said `completed_runs` and `AgentCompletedRunSummary` for what `runs.finished` holds. Both now say finished. The field is off the workflow contract and only the gateway reads it.
- `d02e9ae9`, `f821e7f3`: errors testing `RunStatus::Running` no longer say "active runs", the batch guard no longer says defer, and the leftover comments, locals and test names follow. The CLI's `session_active` had no caller behind its `allow(dead_code)` and is gone.
- `2773e9b3`: the live assertions that read `status == Idle` had become `lifecycleStatus == Open`, which drops the check that no run is active. `bots_live`, `sessions_live` and `tenancy_live` also assert `activity == Idle` again. These compile but have not run.

Corrections to the entry above: the commits named for the session status removal are not where all of it landed. Because of the shared checkout, the `SessionStatus` enum, `api_projection::session_status` and the view fields went in with `73f21af0` and `25b5be9d`, and the bot mapping with `ffdeda61`. So reverting `0cee4709` alone does not bring the old status back. The entry's line on the mapping is fixed, and the rest is recorded here.

Left as they are:

- The wire still calls a context entry id `item_12`, from `api_item_id`, under the API's `ItemId` alias, and the CLI guide documents that form. The judge's "these renames only" did not list it, and it is opaque to clients, so this is the owner's call. It stays cheap to change until integrators keep such ids.
- `lightspeed.core.run.completed` and `RunStatus::Completed` keep their names, because they mean success. The `session/runs/completed` notification has no sender outside `crates/api`'s own test, like the `session/status/changed` it sat beside, so whether it stays is a dead-surface question for the owner rather than a rename.

Verified: `cargo check --workspace --all-targets --locked`; `cargo test -p harness -p tools -p cli --lib --bins`; `cargo test -p temporal-workflow`; `cargo test -p temporal-runtime --lib`; `npm run typecheck`; vitest for `clients/typescript`, `platform/configurator-mcp` and `platform/backend`. Nothing ran live, and no clippy. The live suites listed in the entry still have to prove this, against a purged `lightspeed_hardening` database and Temporal namespace.

Handoff:

- `format-guards`: also `completion_deadline_at_ms` on the invoke effect.

### `format-guards` (H-74, H-75, H-55)

Landed:

- `8d336b2e` (H-75): `SESSION_CHECKPOINT_FORMAT_VERSION` moves next to `CoreAgentState` in the harness, because the shape test has to read it, and is bumped from 1 to 2. That is the one bump for the whole branch and covers every handoff addressed to this package. `advance_checkpoint`, in both stores, replaces a pointer of another format at an equal `through_seq`, so a session idle at its head heals on its next load instead of replaying on every load.
- `0a4ea6b0` (H-75, H-55): `stored_state_keeps_its_committed_shape` compares a fully populated state with `crates/harness/tests/fixtures/checkpoint_state.json`, and its failure names the changed paths and says to bump or regenerate. A `#[cfg(test)]` JSON round trip at the end of `apply_event` runs for every event in every harness lib test. `a_terminal_notification_survives_a_checkpoint_while_the_run_is_active` reaches `notify_on_terminal`.
- `133836e8` (H-74): lenient decoding of recorded payloads, with tests decoding each payload from its identity alone. `continuations_require_explicit_preparation_state` pinned the old strictness, so it now pins the new rule.
- `24be0109` (H-74): `fail_task_if_undecodable` panics on `ActivityExecutionError::Serialization` at session bootstrap, in the bot helper and on the conversation's activity paths. The SDK reports the panic through `fail_wft`, read from its source. The controller and conversation decode tests landed here because they share files with this change.
- `6e92e687` (H-74): the session log rule is stated on `CoreAgentEvent`. `crates/harness/tests/recorded_log.rs` decodes and reduces a 40-event log recorded on this branch, and pins that an unknown event is a decode error.

Calls made:

- The golden's state is built by hand with no `..`, about 1,100 lines of test. Most state fields are optional and skipped when empty, so a golden over a reduced log would never see a new one. Here a new field stops the test compiling. A new enum variant does not, so variants are covered only where a list or map could hold them; plain unit enums show one value.
- Regeneration is an environment variable on the test, `LIGHTSPEED_REGENERATE_CHECKPOINT_FIXTURE=1`, not a tool. It rewrites the copy whether or not the version was bumped, because a field only new event kinds can fill needs no bump. Judging that stays with the author and review.
- Every existing serde default stays, overruling the judge. No Temporal replay tool, no release-tied invalidation, no aliases, and `BlobRef` decode is untouched.
- The recorded log was generated by a throwaway test, which is not committed. It covers lifecycle, tool config, environment, context, run, turn, tool and compaction events. It has no promise, approval, workflow-tool or code-tool events, to stay small, so a rename of one of those would not fail it.
- The workflow contract was re-exported and did not change.

Checked with mutations, then restored: `#[serde(skip)]` on `ActiveRun.notify_on_terminal` fails the round trip, a renamed key in the checkpoint copy fails the golden, and a renamed variant in the recorded log fails with its what-to-do text.

Not done:

- `upsert_bot_trigger_schedule` still does not refresh a Schedule's stored action, which the judge suggested and the package did not list. Fire args decode from their identity alone, and a breaking release's new namespace recreates schedules, so nothing depends on it today.
- Session activities after bootstrap and `BotTriggerFireWorkflow` keep their failure branch on a payload that will not convert. Only the listed sites changed.
- Types nested inside the carries and recorded state, such as `BotDelivery`, `WaitingLane` and `ActiveRun`, keep their required fields. The stated rule covers new fields on them, and no decode test does.

Checks: `cargo test -p harness` passes (419 lib, 16 `state_bounds`, 2 `recorded_log`). `cargo check -p temporal-workflow -p temporal-runtime -p store-pg --tests` passes. The new tests in `temporal-workflow`, the flipped continuation test, and the new `store_pg_live` assertion were compiled and not run, because this package's scoped checks stop at `cargo check` for those crates. The next checkpoint is their first run.

Handoffs:

- Final gate: `sessions_live` (its checkpoint test now reads the moved constant), `store_pg_live` (the checkpoint pointer test gained the equal-sequence case), and the session rollover and bot suites should prove this package. No live suite has run against it.
- Owner or docs: closed by `6134edbd`, which put the log rule, the recorded-payload rule and the checkpoint bump in `docs/documentation/development/changing-contracts.md`. The report of this package still listed it as open.

Independent review of this package found one gap, fixed, and a few smaller things:

- `e6862cbf`: the run start in `controller/lanes.rs` calls its activity directly for its own options, so it missed the panic the helper got. An unreadable `BotStartRunResult` still counted as an outage and took the delivery's retry path. The panic text now also names the result type, because a stalled execution shows nothing else.
- `a6201cc9`: a test of the path the pointer fix exists for. An idle session with a checkpoint of another format at its head replays once, replaces the pointer at the same sequence, and the next load replays nothing. The store tests covered only the pointer, and the `sessions_live` fallback test moves the sequence forward.
- `774f788d`: the recorded-payload comment said identity containers default everything else, but `AgentSessionArgs.session_config` stays required for its model. The round-trip comment said every test checks it, but only this crate's unit tests compile it.
- The guide sentence on unconvertible results read as covering every activity. It now names the sites, and says later session activities and trigger fires still take their failure branch. This is one more wording change for the owner's check.

The report otherwise holds, by reading plus the checks below:

- The panic reaches `fail_wft`: the SDK wraps each routine poll in `catch_unwind` (`temporalio-sdk` `workflow_future.rs`), the conversation's lanes are polled inside it, and no profile sets `panic = abort`.
- All conversation activity calls go through `activity`, `step`, `prepare_media` or `run_typing`, and each converts. `BotTriggerFireWorkflow` is the only other direct call under `bots/`, as recorded.
- `populated_state` has no `..`. Every state-reachable harness struct is named in it; the ones it does not name are events, effects or commands. `RunConfig::default()` on the queued run is not a hole, because the active run names all its fields.
- The bump is once, at 2, and the fixture holds 2. Both stores use the same replace condition. No aliases were added, and no item ids reached source.
- `ConversationState` takes a container default although it holds `trigger_id` and `conversation_key`. `ConversationState::restore` compares them with the start and refuses, so a carry that lost them fails the conversation start loudly and runs under no made-up name. That is why it differs from the identity containers.

This review ran what the package only compiled. `cargo test -p temporal-workflow` passes (255 lib, 3 `workflow_contract`), including every new decode test and the flipped continuation test. `cargo test -p channels -p bots --lib` passes, and `cargo test -p temporal-runtime --lib checkpoint` passes 6. The `store_pg_live` assertion has still not run, and no live suite has. `export-workflow-contract` produced no diff.

### `docs-and-review-notes` (H-73)

Landed: `54ae3cfd` is the one code-free documentation commit, on 32 Markdown files. No Rust, TypeScript or generated file changed. The owner's decision on H-73 is no replay tool, so nothing was built:
the upgrade doc now says compatibility exists only when a release claims it. Scoped check: `npm run check:docs` passes (58 pages, links, anchors, diagrams, search). No live test applies to wording.

Calls made:

- Every handoff addressed to this package was taken, once the review below added the in-flight limit and the `format-guards` wording. The exceptions are `docs/roadmap` files that still describe `store-fs` as it was, since those are history, and the web demo's simulated compaction timing, which is code.
- "Native MCP" became "runtime MCP" in guides, and "managed" became "declared" where it meant the creation path. "Managed sessions" stays where it is web copy, as `rename-vocabulary` recorded, and the Execution form labels stay as the form has them.
- The guides drop the "historical sessions with an omitted compaction policy" text. Admission always fills the policy, and with no old data to replay that sentence describes nothing.
- The self-hosting recipe drops `--stop-signal SIGINT` and keeps `--stop-timeout 30`, because SIGTERM now does the same job.
- `platform/connectors/README.md` named capabilities that no longer exist, so it now lists the method groups the configuration guide already lists. That was not in any handoff.
- `changing-contracts.md` now tells authors the harness counts as workflow code, as the H-73 fact-check asked, and states that there are no `patched()` markers.

Deviations: the `crates/harness/README.md` "projection" line, which the H-41 fact-check called optional, stays as it is, since its scope was guides.

#### Documentation changes for review

All in `54ae3cfd`. Rust reference text is listed under each package above, and in full under `api-reference-corrections`.

| File | What changed |
| --- | --- |
| `deployment/upgrades-and-recovery.md` | New opening with no compatibility promise. New sections: Decide whether the release breaks open work, Move a breaking release to a new namespace (with the table of losses), Restart a stopped session. Procedure gains the script step and an explicit drain. SIGTERM, Recreate, and the fresh database and namespace this branch needs. Two failure rows. |
| `how-it-works/agent-loop-and-durability.md` | Held starts and `operationPending`. New sections: Failures end the run, Keep reduced state bounded. Stopped executions and restart. The compaction hold, pushed promise timing and transport retries are corrected. |
| `how-it-works/context-and-storage.md` | Model switching within a route. Tool groups render adjacent. Catalog retention. Capacity resolved per run. Compaction triggers, backoff, the 1,500 and 2,000 entry rules, largest-entry naming and refusal at the door. `session/read` from the store. Long run reads. Typed roots. |
| `how-it-works/tools-and-controller-workflows.md` | Per-call dispatch and the batch activity for the rest. What waits for an approval. Honest terminal notifications. Dead bot sessions. Branching text removed. |
| `using-lightspeed/sessions-and-runs.md` | New sections: When a run fails, If a session's workflow stops. Max turns. Compaction rewrite. Model switching. Failed starts. Forks and clones removed. Three symptom rows. |
| `using-lightspeed/tools-and-mcp.md` | Runtime default and at-least-once. Live records. Degrade and refusal rule. Last list for 60 s. 64 KiB cut. Approval order. |
| `using-lightspeed/models-and-credentials.md`, `profiles-and-instructions.md`, `cli.md` | Any model within a route. `modelDefaultUnset`. Forced tool choice note. |
| `using-lightspeed/bots-and-triggers.md` | Delivery retries and refusals, degraded bots, retention, dead sessions, steering a parked run. `runFailed`. Two symptom rows. |
| `using-lightspeed/workspaces-and-skills.md` | Per-file conflict. `apply_patch` plans first, and its limits. |
| `using-lightspeed/subagents-and-federation.md` | The 15-minute outage bound. |
| `environments/processes-and-jobs.md`, `power-and-cleanup.md` | What Stop run does to a command. The returned handle. `environmentNotReady`. |
| `environments/using-environments.md` | Default-environment fill and refusal. |
| `integrating-and-extending/workflow-tools.md` | `session/declared/start`, `workflowToolId`, `schemaRevision`, `recipeRevision`, `workflow_tool_id`, `workflowToolInvoked`. Honest `notifyOnTerminal`. Dropped signals and failed replies. |
| `integrating-and-extending/api-and-typescript.md` | `gap` removed. Run `failure`, session view fields, `earlierEventsOmitted`, mutation summaries, `blobRef` and `mime`, camelCase kinds, `operationPending`, `sessionExecutionStopped`, retry windows, failed starts. |
| `integrating-and-extending/configurator-mcp.md`, `custom-tools-and-model-providers.md`, `platform/configurator-mcp/README.md`, `platform/README.md`, `platform/connectors/README.md` | Vocabulary and camelCase values. Connector method groups. |
| `deployment/operations.md` | Reaper reporting and `promise_repair_repeated`. Retention warning. Never-opened children. Sweep position and `walks_completed`. SIGTERM. The 45 s close. |
| `deployment/troubleshooting.md` | Stopped sessions and restart. Nondeterminism. Failure kinds and `operationPending`. The entry limit. |
| `deployment/self-hosting.md`, `configuration.md`, `reference/environment-variables.md`, `access-and-security/people-and-roles.md`, `private-and-shared-work.md` | Stop flag, vocabulary, forks removed. |
| `development/changing-contracts.md`, `contributing-and-releasing.md`, `local-development.md` | The patching sentence is gone. The script in the release steps. Reset leaves Temporal alone. `6134edbd`: stored format and recorded payload rules, and how to create a dev namespace. |
| `scripts/dev/README.md` | `6134edbd`: says no workflow attaches the search attributes it lists. |

#### Wanted wording that was not edited

Root `README.md`:

- Line 131 lists forks and clones as shipped, and they are not exposed. Delete `- [x] **Session fork & clone primitives**: share stored history for branches or start from copied configuration`.
- Optional, in the same list: `- [x] **Session restart**: a stopped session comes back from its log, and an interrupted run fails rather than repeating its work`.

`AGENTS.md`:

- Architecture Boundaries, "Managed sessions and workflow-backed tools use the generic workflow-tool protocol." becomes "Declared sessions and workflow-backed tools use the generic workflow-tool protocol."
- Generated Artifacts could gain: "A change to `temporal-workflow`, its workspace dependencies or the Temporal SDK breaks open executions. There are no `patched()` markers: `scripts/release/workflow-compat.sh` says whether a release is breaking, and a breaking release starts on a new Temporal namespace."

#### Items left undone

Set aside by the review and not overruled:

- S-01: `PatchTools` and `ReplaceTools` both stay. Hosted publishing has one path, so nothing goes wrong.
- S-02: code tool identities stay bare strings. No wrong result.
- S-03: context-invalidation policy stays in the Temporal host. No wrong result.
- S-04: `wait_for_workflow_work` leaves its timer running when state wakes first. Harmless beyond a little history.
- S-06, S-07: component naming and the `CoreAgent` prefix stay. Both are private vocabulary that can change at the same cost later.
- S-09: the CLI still links `harness`, for `BlobRef` only.
- S-10: `run_process` mints a new id per dispatch. `tool-dispatch-per-call` removed the automatic re-dispatch and the residue never holds it, so nothing reaches this.
- S-11: nothing bounds how long dormant executions need their history replayable. Moot under the new namespace rule.
- R-01 to R-05: refuted, no action.

Set aside, but done anyway as part of other work: S-05 fell out of H-70, because native MCP calls left the batch path, and S-08 landed in `058b6ac1`.

Rejected by the owner, so not built: H-87 (`apply_patch` still overwrites on add and move, "let the model cook") and H-89 (no recovery for stalled controllers and conversations, and no rescan of undelivered events, since bots are work in progress).

#### Follow-ups

- **ls-benchmark.** `test_agent_run.py:457` reads an old error kind, and `src/lightspeed_harbor/agent.py:875` reads `completedAtMs`, which silently goes empty until it moves to `finishedAtMs`. That repo may also use `session/managed/start` or `execution: native`.
- **Paged run detail.** `session/runs/read` keeps the opening and latest turns of a very long run, and every read still pages its whole interval.
- **Pruning old VFS heads.** Every head a session committed stays rooted until the session is deleted.
- **Sub-agent default deadline.** `SUBAGENT_DEFAULT_DEADLINE_MS` (1 hour, 24 h ceiling) still awaits an owner decision, against "no default limits". Removing it needs promise and child lifecycle changes. Resolved 2026-11-04: `run_timeout_ms` is optional with no default or ceiling, `agent_run` and `agent_spawn` carry no hard promise deadline, and a 15 minute liveness check fails the delegation when the child's session workflow stops.
- **An index for log-backed run pages,** if deep paging is ever slow. `read_run_record_entries` filters `session_events` with no index of its own.
- **Usage on the terminal event,** which would give old run summaries and compaction a usage figure. See Risky calls.
- **The no-object-store retry.** A typed variant for "no object store is configured" would stop a blob over the inline limit retrying for 21 minutes on a PostgreSQL-only deployment.
- **`failed to initiate panic, error 5`.** A failing test can abort a whole `temporal-runtime` test binary on macOS and hide its message. `--nocapture` or `--exact` works around it. Never investigated.
- **`LiveConfigurator` leaves its node child running,** which holds stdout open, so `cargo test ... | tail` never returns on `mcp_live`. Write the output to a file.
- **`session/runs/completed` and `item_12`.** The notification has no sender, and the wire still calls a context entry id `item_12`. Both are the owner's call, and cheap until integrators keep them.
- **Unused search attributes.** `scripts/dev/infra/temporal-ensure.sh` registers five `Lightspeed*` attributes that no workflow attaches. Delete the registration, or attach them if the Temporal UI should find conversations by them.
- **Roadmap files.** `docs/roadmap/later/pNNN-local-runtime-without-temporal.md` and `p185-external-harness-sessions.md` still describe `store-fs` as it was.

#### Live suite results

Live suites ran on the dev stack, in a separate `lightspeed_hardening` database and Temporal namespace. Nothing touched the owner's dev data.

Before checkpoint 1: narrow runs passed for `session-liveness-read` and `core-failrun`. From `forks-and-kind-constants` to `typed-blob-refs` the Docker daemon was down and starting it was out of bounds, so nothing ran. The first checkpoint attempt failed every suite on connection errors, which said nothing about the code.

Checkpoint 1, after `typed-blob-refs` and its fixes (`cp1` and `cp1b`). All pass: `store_pg_live` 42, `audience_live` 1, `runs_live` 10, `sessions_live` 16, `mcp_live` 5, `subagents_live` 9, `workflow_tool_plugins_live` 17, `code_tools_live` 4, `code_workflow_live` 15. `session_lifecycle_live` ended 4 of 5 in both logged runs: the retention test expected the old error variant. Its fix landed with no logged re-run at that checkpoint, and the suite passes 5 at checkpoint 2. Other failures found on the way are in that section above.

Narrow runs between the checkpoints, all passing: `pg_live_run_record_entries_are_read_by_run_id_across_a_fork`; `anthropic_messages_live_runtime_reports_input_overflow_as_context_limit`; `run_capacity_live`; `temporal_live_command_waits_answer_from_their_own_outcome` and its neighbours under `api-waiters`; `profiles_live` and the `config-admission` tests; `temporal_live_native_mcp_search_call_and_approval`.

Checkpoint 2, after `session-restart`, `reaper-and-retention` and `bots-dead-session`:

| Result | Suites (tests passed) |
| --- | --- |
| Passed first time | `store_pg_live` 44, `session_lifecycle_live` 5, `audience_live` 1, `bots_pg` 5, `api_keys_live` 1, `model_defaults_live` 1, `universe_slugs_live` 1, `runs_live` 12, `sessions_live` 17, `session_restart_live` 5, `mcp_live` 6, `mcp_oauth_live` 1, `subagents_live` 10, `workflow_tool_plugins_live` 17, `bots_live` 10, `channels_live` 4, `profiles_live` 3, `code_tools_live` 4, `code_worker_process_live` 1, `codemode_live` 7, `authentication_live` 1, `environment_provider_live` 2, `environment_registration_live` 1, `vfs_transfer_live` 1, `transcriptions_live` 3, `anthropic_messages_mcp_live` 1, `openai_responses_mcp_live` 1, `run_capacity_live` 1, the three provider compaction suites at 4 each, `openai_responses_live` 9, `openai_completions_live` 13 |
| Failed, then fixed and re-run | `code_workflow_live` (15, read pruned state; 15 pass after), `code_capabilities_live` and `code_model_live` (same cause, then a schema fix), `authorization_live` (aborted; passes after), `tenancy_live` (no model default on its new universes, `77211528`; passes after), `anthropic_messages_live` (6 fixture failures of 24, see Checkpoint 2 above; 27 pass after, with the tests that commit added) |
| Failed, not re-run | `migrations_live`: its schema revision pin was stale at that commit. The pin was moved to 14 in `73f21af0`, so the final gate has to show it. |

Static at checkpoint 2: `cargo test --workspace` had one failure, a manifest description over its limit, fixed in `11ee3c3e`. Clippy failed on lints, fixed in `f766f8b9`, `ea8e539d` and `1903fbed`. TypeScript typecheck, consumer tests and `verify-metadata.sh` passed.

Not run in any checkpoint:

- `runs_live_slow`: takes about 30 minutes, and project rules ask for confirmation first.
- `incus_live`, `azure_blob_live`, `postgres_tls_live`: need Incus, Azure and a TLS Postgres, none of which the dev stack has.
- `anthropic_messages_caching_live`: changed at checkpoint 2 and compiled only. The other provider `*_caching_live`, `*_prompts_live` and `*_skills_live` suites, `openai_audio_transcriptions_live`, `builtin_catalog_parity`, and the `codemode` crate's `concurrency_live`, `content_output_live` and `filesystem_live` were not on the checkpoint's list. No reason is recorded.

After checkpoint 2, no live suite has run against `api-reference-corrections`, `rename-casing`, `rename-vocabulary`, `rename-wire-identities` or `format-guards`. Those are the widest changes on the branch, and only the final gate can prove them.

#### Merge notes

The branch's stored formats require a reset and a new namespace at merge. Run `./dev.sh reset` for the dev databases, or any purge of the runtime database, then `cargo run -p temporal-runtime -- migrate` to schema revision 14, and point `TEMPORAL_NAMESPACE` at a new namespace on the runtime and connector host. Create it first with `docker exec lightspeed-temporal temporal operator namespace create --namespace <name>`, because `./dev.sh` fails waiting for one that does not exist. Old logs, checkpoints, histories and bot rows do not decode, and migrations 013 and 014 refuse old bot and MCP values. The owner has said the local dev database may be reset for live tests. An existing deployment needs the same fresh state. `scripts/release/workflow-compat.sh` prints `breaking` for this branch.

#### Independent review of `docs-and-review-notes`

Checked the 32 files against the code: the script's crate list against `cargo metadata`, the sample `session/declared/start` body against the Rust input types, and the numbers, method groups, error kinds, log events, web labels and restart paths against their source. Also compared the checkpoint tables with the checkpoint logs, and listed every `#[ignore]` suite against the results above. `npm run check:docs` passes. No Rust, TypeScript or generated file changed in either pass, and the root `README.md` and `AGENTS.md` are untouched. Most of the wording held up. Fixed in `6134edbd`:

- The namespace section told operators to copy five custom search attributes, and the notes above said a namespace without them fails conversation starts. No code in the runtime or the connector host attaches them; only `temporal-ensure.sh` registers them. The requirement and the claim are gone, and `scripts/dev/README.md`, which said conversations attach them, is corrected.
- Custom workers were left out of the namespace move. A workflow tool whose worker stays behind starts no work. The old namespace's bot schedules are now mentioned too.
- The recovery table still said PostgreSQL is no replacement for lost Temporal state, which contradicted the restart section.
- `format-guards` landed beside this package, and its handoff to the docs was open. `changing-contracts.md` now has the log rule, the recorded payload rule and the checkpoint bump, and no longer implies stored events are left behind with open executions. The upgrade and troubleshooting pages name the payload panic beside the nondeterminism error.
- The workflow tools guide had no in-flight limit, though `state-pruning` handed it off and the notes said every handoff was taken.
- The dev steps said to point at a new namespace, which fails after 60 seconds in `temporal-ensure.sh` unless it was created. The command is now there. The CLI's default address was confirmed against the dev container, read-only. Nobody has run the whole sequence.
- The live results said `session_lifecycle_live` passed 5 at checkpoint 1. Its logs say 4 of 5, corrected above. `builtin_catalog_parity` was missing from the suites not run.

Left alone: `tools-and-mcp.md` describes a `codex_like` presentation the public API does not expose. It predates this branch and is a separate question. `queue_key` in `processes-and-jobs.md` is the tool argument, not the API's `queueKey`, and is fine there.

Live tests: none apply to wording. The final gate's `migrations_live` and the suites this package named still stand.

## Final gate (2026-11-04)

Static: `cargo clippy --workspace --all-targets --locked -- -D warnings` is clean, `cargo test --workspace` passed 3,081 tests, and `npm run check` and `scripts/release/verify-metadata.sh` pass. A small refusal fix landed after the workspace run, so for that commit clippy, `cargo check` and the `llm-runtime` compaction unit tests were re-run, not the whole suite.

Live: the dev database was reset with `./dev.sh reset`, and the suites ran against it in a new Temporal namespace, `lightspeed-final`, which is the upgrade procedure from H-68. 40 of 41 suites passed, 250 tests in all, including every Temporal suite after the renames.

Not run: `runs_live_slow`, by choice; `openai_responses_caching_live`, `openai_completions_caching_live`, and the provider prompt and skills live suites, which no package changed directly; and `azure_blob_live` and `postgres_tls_live`.

The one failing suite is `anthropic_messages_compaction_live`, and it is not caused by this branch. Anthropic's classifier refuses its fixtures at random. Across seven runs the refusals named `reasoning_extraction`, `cyber` and `bio` on `claude-opus-5-5`, `claude-sonnet-5` and `claude-sonnet-5-5`, and different tests failed each time. The same suite passed 4 of 4 at the previous checkpoint, and the changes since then are renames, not prompt wording. A refused native compaction used to report `invalid provider response` with no category, so it now reports `ProviderRefused` with the category and explanation.

### Read these first

These are the calls most worth your attention. Full detail is under 'Risky calls for review'.

1. **The branch is a breaking release.** Stored event kinds, durable state and wire values changed, with no compat layer. Merging needs fresh databases and a new Temporal namespace, including on any existing AKS deployment.
2. **Refused compactions on Anthropic may be a real product risk.** This came out of the final gate and no decision covers it. Refusal stays a failed turn or failed compaction with no fallback, as you decided earlier. If the classifier does this to real sessions, a long-running agent could hit repeated compaction failures. Worth watching before relying on Anthropic for very long runs.
3. **Per-call dispatch (H-70)** reopens your September call on mixed batches. A spawn beside an approval-gated call now waits for the decision.
4. **The 2,000-entry guard (H-85)** is technically a limit, against 'no default limits'. It fails a run only once compaction cannot get below it, because the session would otherwise become unloadable near 3,000 entries.
5. **Pruning windows (H-57):** 256 finished runs, 256 finished promises and 64 steering batches. A submission retried more than 256 runs later starts a new run.
6. **FailRun covers store data rejections (H-60 with H-13).** A real store bug costs one run and gives an error log, not a failed workflow.
7. **Restart (H-64)** fails an interrupted run and runs only on request. A bound receiver's reply sent during an outage is lost.
8. **MCP (H-83, H-84):** new servers default to native, and an admin's URL or auth change applies at once, including to an already approved call.
9. **Bots (H-68, H-69):** a breaking release resets conversations, undelivered events, budgets and group `/activation`. A delivery reaching a dead session rolls forward without its context.
10. **The sub-agent default deadline** (1 hour, 24 hour ceiling) is a default limit no decision covered, so it was left as is and needs your call. Resolved 2026-11-04: `run_timeout_ms` is optional with no default or ceiling, `agent_run` and `agent_spawn` carry no hard promise deadline, and a 15 minute liveness check fails the delegation when the child's session workflow stops.
11. **Validated `BlobRef` decode (H-18)** stays strict despite H-74's leniency, because typed blob roots depend on it.
12. **Documentation is unsigned.** All guide and upgrade changes went in without your sign-off, in code-free commits, for you to review. `AGENTS.md` and the root `README.md` were not edited, and the wording they need is listed in this document.

### Sub-agent limits, after the final gate

The owner dropped the lifetime `maxDescendants`: it stopped long-running agents and protected nothing `maxConcurrent` does not, since a child closes when it finishes. `maxConcurrent` is the safety limit, now defaulting to 8 across the tree. At the limit the call is refused and tells the model to await a running child; it does not queue. A run that spawns one child at a time can go on indefinitely, which matches the rule that limits apply only when set. `job_run` and the LLM call limits stay as they are, by the owner's call.

### Parallel work

The last two groups ran in one checkout with agents working at once, as you asked. Commits interleave, and some carry a neighbour's edit to a shared file, so cherry-picking a whole package from those groups is harder than from the first ten. For example, the session status removal in H-25 spans commits from two rename packages.

### At merge

- Run `./dev.sh`, then `lightspeed connect dev`, because the reset cleared the saved development credentials.
- The default Temporal namespace still holds 31 open workflows from before the reset. They will fail on replay under this code. Terminate them, or use a new namespace.
