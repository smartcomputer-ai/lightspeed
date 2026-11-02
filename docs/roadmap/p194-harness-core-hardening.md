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
  costs one command and no longer produces a failed workflow signal.
- **Reducer trust moved to the proposal layer.** With policy out of the reducer, the
  dry run no longer catches a proposal builder that skips a moved check: a builder
  bug can now commit a policy-violating event that replays fine. Moved checks that
  held by construction have no second line of defence.
- **A queued message during a long parked batch delays rollover.** The toolset of a
  run prepared while a batch is parked stays pending, and `pending_toolsets` blocks
  continue-as-new. History only grows by what the park itself produces, but a run
  parked for days holds the rollover until the batch ends.

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
