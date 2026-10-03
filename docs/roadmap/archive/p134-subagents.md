# P134 — Sub-agents: Governed Delegation on the Workflow-Tool Spine

**Status**

- Slices 1–3 implemented 2026-08-25: schema folded into the pre-release
  `001_core` baseline (`session_links` omitted,
  `origin_json` + two indexed keys, transactional root-scoped reservation), `SessionOrigin`
  on views and `session/list` filters, `features.subagents` replacing the
  fleet family across engine/api/projection/contract/TS/web,
  `SubagentExecutionWorkflow` with prepare/resolve/close activities, the
  `agent_run`/`agent_spawn` system bindings with admission-time context
  pinning, fleet control plane and `PromiseSource::Run` removed, live
  scenarios (inline result, three-way fan-out, root-limit refusal,
  parent-cancel closes child) replacing the fleet suite.
- Slices 4 and 5 implemented 2026-08-25: `SubagentCatalog` context entry
  (`subagents.catalog`) built from the grant and current profile records,
  published by the runtime projection before each run and on idle reads,
  rendered by every provider adapter; `ProfileEnvironment::Inherit` resolved
  from the child's origin at apply time (rejected off a delegation origin or
  when the parent has no active environment), in the profile editor and CLI
  validation; live scenarios assert the catalog entry and that an inherited
  provisioned environment stays open after the child closes.
- Slice 7 implemented 2026-08-25: the mailbox is gone — `await { mailbox }`,
  `SubmitMessage`, the `MessageBuffered`/`MessageConsumedByAwait`/
  `MessagePromotedToRun`/`MessageCancelled` events, `WakeReason::MailboxMessage`,
  `RunOrigin`, and the buffered-message state. The detached-promise follow-up
  wakes an idle session with an ordinary `RequestRun`.
- Lineage surfaces done 2026-08-25: the sessions list groups sub-agents
  under their parent (badge, indent, "show sub-agent sessions" toggle) and
  the session view carries a lineage strip (parent link, pinned profile,
  depth; live children chips re-read on every run revision); the platform
  session proxy forwards `rootSessionId`/`parentSessionId`. Bots: the
  retention sweep closes a routed session's open descendants first (a busy
  descendant defers the close), the activity feed records how many, and
  `GET /bots/:id/state` returns per-session `lineage` (open/total counts and
  a bounded child list) that the bot detail renders.
- Budget and tests done 2026-08-26: the bot's `runsPerDay` now counts
  sub-agent descendants like runs — the controller re-reads today's
  descendants by lineage root (`countBotDescendantSessions`, after every
  finished delivery and once a minute while a run is in flight; best effort,
  the last count stands during a core outage), keeps them in `reservedRuns`,
  the snapshot (`descendantsToday`), the continue-as-new carry, and the
  `budget_exhausted` detail; the bot detail shows the split. Unit tests
  landed for the execution workflow's pure signal/recovery logic
  (`classify_emission`, `recovery_result`), `SubagentService`
  prepare/resolve/close against a counting fake runtime, the in-memory
  store's root-scoped reservation and lineage list filters, and `agent_run`
  admission (grant required, catalog allowlist, pinned context, idempotent
  retry). A live deadline scenario
  (`temporal_live_agent_run_deadline_fails_the_reply_and_closes_the_child`)
  joins the sub-agent live suite; `maxDescendants` live was already covered
  by the root-limit scenario. Nothing in this document remains open;
  everything under Non-goals stays deferred by design.
- Proposed 2026-08-25, from the fleet-vs-bots review
  (`later/pNNN-fleet-vs-bots.md`, direction C′) and a design discussion with
  Lukas the same day. Decisions taken there and fixed here: the agent menu is
  a refreshed catalog context entry, not tool schema; two tools shaped like
  `job_run`/`job_submit`; children are started through a start-on-call
  execution workflow, not a parent-side transport; children are one-shot
  with the run-owned lifetime reserved; environments are inherited by profile
  intent; `label` is the child's display name.
- Supersedes the fleet control plane (P82–P85, P92's fleet half, the one-off
  lifecycle appendix) and absorbs the never-built P93 safety layer. Builds on
  P100/P100b/P106 (workflow tools, start-on-call recipes, joined completion),
  P125/P126 (environment intents, power), P129 (run control), P132 (the
  generated workflow contract the execution workflow speaks).
- Greenfield: no wire, schema, or event back-compat is kept. Dev and test
  databases are purged; the contract regenerates; the fleet feature, its
  tools, and `session_links` are removed rather than deprecated.

## Why

Lightspeed tells two stories for "more than one session's worth of work".
Bots (P130) are *durable orchestration*: a deterministic controller admits
events, routes, budgets, and drives managed sessions for months. Fleet
(P82–P92) is *attached delegation*: a run fans work out to children and joins
their results inside one reasoning loop. The survey in the fleet-vs-bots
review found the second story shipped as a 5.4k-line, seven-tool control
plane with one consumer (the retired Foundry manager), no budgets, a graph
the model could see but the human could not, and a shipped schema
advertising tools that no longer exist.

Attached delegation is still wanted — but as a **profile-expressible core
capability**, because profiles are the template mechanism (one profile,
many stamped sessions) while bots are named singletons, and workflow tools
exist only on managed sessions. "Wrap it in a bot" cannot serve an
interactive session or a profile meant to be instantiated many times.

The design insight that makes the rebuild small: the session workflow
already speaks **one protocol to durable work that finishes later** — a
promise, a producer identity fixed at admission, a `source_resolution`
emission. Environment jobs were the first such producer (`job_run` /
`job_submit` over `EnvironmentJobWorkflow`). Sub-agents are the second. A
child session is made to look like a job by one small execution workflow,
and the parent session workflow, the engine, and the API gain no sub-agent
code at all. Fleet's hard-won pieces — deterministic child ids, starting the
child, run-terminal notification — move into that workflow's activities
instead of being rewritten.

## The shape

```text
 parent session workflow                SubagentExecutionWorkflow            child session workflow
 ───────────────────────                ─────────────────────────            ──────────────────────
 agent_run { agent, input }
   engine mints `reply` promise
   batch parks (JoinedWorkflowCalls)
   start-on-call ─────────────────────▶ A. prepare (activity)
                                           validate grant, reserve tree slot,
                                           create child from pinned profile,
                                           resolve `inherit`, start run ────▶ runs like any session
                                        B. wait on signals                    …
                                           ◀──── run_terminal (notify intent) ┘
                                        C. resolve (activity)
   ◀──── source_resolution { reply }       build result envelope,
   batch resumes, results per call         close child (activity)
   next LLM turn
                                        cancel (any cause) ──▶ close child
```

Three `agent_run` calls in one turn are three executions; the batch resumes
when all three `reply` promises settle, with one tool result per call.
`agent_spawn` is the same path with a promise handed back for `await`.

## Design

### 1. Model surface

Two tools, shaped exactly like the environment-job pair, plus the generic
concurrency tools already in the toolset. Every argument is a plain type;
nothing about the menu lives in the schema.

```text
agent_run   { agent: string, input: string, label?: string }
  -> joined: the call's result is the child's result envelope; N calls in
     one turn park together and resume together (fan-out / fan-in)

agent_spawn { agent: string, input: string, label?: string }
  -> { accepted, promise }; join with await/cancel/detach. The child's
     session id and run id arrive in the result envelope (P138).

await { promises, mode: all|any, timeout_ms }   unchanged
cancel { promises }                              unchanged; a child promise's
                                                 cancellation closes the child
detach { promises }                              unchanged; the child outlives
                                                 the run, its result waits in
                                                 the session-scoped promise
sleep { ms }                                     unchanged
```

Result envelope, returned inline by `agent_run` and as the `await` payload
for `agent_spawn`:

```text
{ agent, sessionId, runId,
  status: completed | failed | cancelled | deadline,
  output?: string, error?: string }
```

- `agent` is validated at admission against the grant's allowlist (a typed
  tool error names the allowed ids) and pinned with the profile revision.
- `input` is the whole brief and arrives as the child's first user message,
  unwrapped. The child's profile instructions own its persona; the fleet
  `fleet_request {from_session_id, payload}` envelope goes.
- `label` becomes the child's `display_name` (store metadata, like any
  session's) so a parent that runs `reviewer` five times reads as
  "reviewer: PR 1234 / reviewer: PR 1240 …" in the sessions tree.
- No `agent_send`, `agent_request`, `agent_list`, `agent_read`,
  `profile_list`, `profile_read`. Children return one result; parents hold
  the ids they created; humans read the graph through the API and UI.

### 2. The grant: `features.subagents`

```text
features.subagents {
  version: 1,
  agents: [ { profileId: "reviewer" }, { profileId: "test-runner" } ],
  maxDepth: 2,            // root-scoped: a child at depth d may spawn iff d+1 <= maxDepth
  maxDescendants: 16,     // root-scoped, lifetime: total sessions ever created under the root
  maxConcurrent: 4,       // root-scoped: open sessions under the root, excluding the root
  deadlineMs: 3600000     // per child run; default 1 h, at most the binding ceiling
}
```

- `agents` is the menu and the authority. Ids must name existing profiles at
  config admission (a put naming an unknown profile is rejected). A profile
  that appears in the universe later is invisible until a grant names it.
- Limits are **root-scoped and attenuating**. A root is any session without
  an origin; every descendant counts against that root. A child profile may
  itself grant `subagents` (nesting); its effective limits are the
  element-wise minimum of its own grant and the effective limits pinned on
  its origin at spawn, so a descendant can narrow its ancestors' limits and
  never widen them. Depth is absolute from the root.
- Counting is a store transaction, not engine state: the prepare activity
  locks the root's session row, counts descendants by origin columns, and
  inserts the child row with its origin in the same transaction. The child
  row *is* the reservation; the deterministic child id makes a retried
  insert a no-op rather than a second child.
- Revoking the grant hides the tools from the toolset on the next config put
  (bindings are immutable; the toolset filter is the same mechanism that
  hides `job_*` when `features.environments.jobs` is absent). Calls admitted
  before the revoke complete normally.

### 3. The menu: a catalog context entry

The repo has two mechanisms for "what the model may pick from": tool
schemas, derived only at start / config put / profile apply and never
refreshed on their own (the MCP tool list has exactly that staleness), and
**catalogs** — skills, prompts, the VFS catalog — which are context entries
rebuilt by `runtime_projection_refresh` before every run admission on an
idle session and on idle API reads, fingerprinted so unchanged content is a
no-op. Sub-agents use the catalog mechanism:

- `ContextEntryKind::SubagentCatalog` under `SUBAGENT_CATALOG_CONTEXT_KEY`,
  one entry per session, published when `features.subagents` is granted and
  removed when it is not.
- Content is rendered from the grant's `agents[]` joined with the current
  profile records: id, display name, and the profile's `description` as its
  "when to use" line (the Claude Code `whenToUse` role; no new profile field).
  Description edits land at the next run; the pinned profile revision at
  spawn still governs the child's configuration.
- The tool schemas stay static (`agent: string`), which is what lets the
  tools be immutable system bindings (§4). `agent` is validated at admission,
  never by enum.

### 4. `SubagentExecutionWorkflow`

Setup, once per session, when the grant is present: the gateway admits two
**system workflow-tool bindings**, exactly as it admits the environment-job
pair — `agent_run` with `WorkflowToolCompletion::Joined { deadline_after_ms:
SUBAGENT_DEADLINE_CEILING_MS }` and `agent_spawn` with
`Completion::Promises { key_source: Reply, max_promises: 1, deadline_after_ms:
ceiling }`, both `Target::Start { recipe: SubagentExecutionWorkflow on the
core task queue }`. The ceiling (24 h) is the hard bound the engine enforces;
the grant's `deadlineMs` is enforced inside the execution and pinned per call,
so the binding never needs to change when the grant does. Sessions that
already admitted the former four-hour binding retain that immutable ceiling;
they must be recreated to use a longer deadline.

Admission (session worker, the `JobSubmitExecutionContextV1` pattern): when a
batch carries an `agent_*` call, the worker validates `agent` against the
grant on the batch request (`subagents_policy`, the renamed `fleet_policy`
slot) and pins a `SubagentExecutionContextV1 { grant: effective limits,
deadline, parent: { sessionId, runId, origin? }, agent: { profileId,
revision } }` into the invocation's `execution_context_ref`. A call with no
grant, an unlisted agent, or a missing profile fails at admission with a
typed tool error; nothing starts.

The workflow, modelled on `EnvironmentJobWorkflow` but simpler (one child,
one promise, no polling loop):

1. **Identity.** Input is `WorkflowToolStartArgs` (holder = the parent
   session workflow id, `execution_id`, the invocation). The workflow id must
   equal `execution_id`; the child session id is `agent_<digest32(execution_id)>`
   and the child's first-run submission id derives the same way, so every
   retry converges on one child.
2. **A. prepare** (activity; `FleetService::spawn` moved, not rewritten):
   read the execution context; in one store transaction lock the root row,
   check `maxDepth`/`maxDescendants`/`maxConcurrent`, create the child
   session from the pinned profile revision with its origin (§5) and
   `display_name = label`; apply the profile (environment intent included,
   `inherit` resolving to the parent's active environment); start the child's
   run with `notify_on_terminal: [{ holder_workflow_id: execution_id, token:
   reply promise id }]`. A limit violation is a terminal failure: the
   execution resolves `reply` as `Failed { error: SubagentLimitExceeded {…} }`
   and exits; no child exists.
3. **B. wait.** `select!` over: a `deliver_emission` signal carrying the
   child's `run_terminal` (done), a `deliver_emission` carrying an
   `invocation_cancellation` from the holder (close child), Temporal
   cancellation (`ctx.cancelled()`, the parent cancelled the promise for any
   reason: model `cancel`, run-terminal cascade, force-close), and the
   grant's deadline timer (close child, resolve `Failed { deadline }`).
   History stays a handful of events however long the child runs.
4. **C. resolve** (activity): read the child's terminal run, write the
   result envelope to CAS, signal the holder `source_resolution { reply,
   Resolved { payload } | Failed { error } }`, then close the child
   (`session/close { force: true }` — the one place a child is closed), and
   exit. Close-on-terminal is not a child-side flag any more.
5. **Cancel path**: close the child first (cancelling its run and, through
   P125 retention, its provisioned environment), then exit `Cancelled`. The
   parent already marked the promise cancelled; no resolution is sent.
6. **Recovery query** (`workflow_tool_recovery`): if asked before step C
   completed, read the child's run status through an activity and answer
   with the resolution if the run is terminal. The parent's existing slow
   recovery poll on `WORKFLOW_TOOL_EXECUTION_KIND` promises is the backstop
   for a lost `source_resolution`; no new polling anywhere.

What the parent side reuses without change: promise minting, the joined
park and per-call resume (`drive.rs` joined path), deterministic start with
bounded retries, `cancel_workflow_tool_execution` on promise cancellation,
producer authorization of the `source_resolution`, the recovery poll, and
the P132 envelope. What the child side reuses: `RunTerminalNotifyIntent` —
the child signals its holder exactly as it signals a bot controller today;
the holder is the execution instead of the parent.

### 5. Session origin (lineage)

`session_links` — fleet's only writer, unenforced and unexposed — is replaced
by typed, immutable provenance on the session record, the P125
`environment.origin_session` pattern:

```text
SessionOrigin {
  kind: "subagent",
  parentSessionId, parentRunId, rootSessionId, depth,
  invocationId,
  agent: { profileId, revision },
  limits: { maxDepth, maxDescendants, maxConcurrent, deadlineMs }   // effective, pinned
}
```

- Store: one `sessions.origin_json` document (the serialized
  `SessionOrigin`) plus the two facts queries need, denormalized and
  indexed — `origin_root_session_id` (reservation counts, `rootSessionId`
  filter) and `origin_parent_session_id` (`parentSessionId` filter) — with a
  shape CHECK (all null or all present); `session_links` omitted from the
  pre-release `001_core` baseline. `source_session_id` / `source_seq` stay what
  they are: clone/fork content ancestry, empty for profile spawns.
- API: `SessionView.origin?` and `SessionSummaryView.origin?`;
  `session/list` gains `rootSessionId?` and `parentSessionId?` filters.
  Origin is set at creation and never changed; it is provenance, not
  ownership.
- UI: the sessions page renders children under their parent (label or
  profile display name, status, depth); a root's page shows its tree.
- Bots: a bot's retention sweep, activity feed, and UI walk
  `rootSessionId ∈ {the bot's sessions}` to include descendants. The bot
  budget stays an *activation* budget until the controller counts
  descendants through the new list filter.

### 6. Child lifetime

Children are **one-shot**: lifetime = the single run = the single promise.

| Event | Effect |
|---|---|
| child run completes / fails | execution resolves `reply`, closes the child |
| model `cancel` on the promise | promise cancelled → execution cancelled → child closed |
| parent run reaches terminal with pending run-scoped promises | promises auto-cancel (structured concurrency) → children closed; a parent needs no explicit cancel |
| `detach` | promise becomes session-scoped; child outlives the run; result waits in the promise; session close blocks on it (or force-close cancels it) |
| grant deadline | execution closes the child, resolves `Failed { deadline }` |
| parent force-closed | session-scoped promises cancelled → executions cancelled → children closed |

Follow-ups (a second run in an existing child, Claude Code's `SendMessage`,
Codex's `followup_task`) are deliberately not built. They need run-owned
children — closed at the parent run's terminal even when idle — which is a
lifetime the promise cascade cannot express and would require an origin walk
at run terminal plus an orphan sweep. `parentRunId` is stamped on the origin
from day one so that lifetime can be added without a migration. The cheap
substitute today is a fresh child on an inherited environment.

### 7. Environments: `inherit`

`ProfileEnvironment` gains a third intent:

```text
{ type: "inherit" }
```

Applied only through a sub-agent spawn: the prepare activity resolves it to
the parent's current `active_environment_id` and activates that environment
in the child exactly as `existing` does. Rules: the parent has no active
environment → typed spawn failure; a profile with `inherit` applied to a
session without an origin → rejected; the child never closes an inherited
environment (as with `existing`); the child profile still needs
`features.environments` to hold an active environment at all. A
session-provisioned environment therefore passes down for free: its
`origin_session` and close trigger stay with the parent, children are gone by
the time the parent closes, and a powered-down environment wakes on the
child's first use through the P126 path. Sharing is sharing — several
children on one environment share its filesystem (process groups stay per
session, P115); choosing `inherit` in a child profile is the operator saying
so. Per-child isolation (snapshot-per-child, the worktree analog) is out of
scope until asked for.

### 8. How bots compose

Nothing in the bot controller changes. A bot's worker profile carries
`features.subagents`; its routed and main sessions become roots; the
controller sees the trees through origin. Bot↔bot remains events
(`bot_emit { targetBot }`, P131 ws6); a child is never a bot and a bot is
never a child. "Durable orchestration above, attached delegation below" is
the whole story, and the two share sessions, runs, promises, and the P132
protocol without either knowing the other's rules.

### 9. Removal ledger

Everything fleet, deleted rather than deprecated:

| Area | Goes |
|---|---|
| tools | `crates/tools/src/fleet/` (all seven tools, args, outputs); `FleetToolsetConfig`; `config.fleet` in `resolve_toolset`; the `VfsPolicy::Isolate` workspace derivation |
| server | `temporal-runtime/src/fleet.rs` except the spawn body that becomes the prepare activity; `AgentApiFleetRuntime`; `start_session_for_fleet_with_profile`, `start_run_for_fleet`, `enqueue_run_for_fleet`, `deliver_message_for_fleet`; the fleet branches and message-dedup path in `worker/session_tools.rs`; fleet wiring in `universe.rs`, `lib.rs`, `main.rs`, `worker/activities/{mod,state}.rs` |
| engine | `FleetFeature`, `FleetProfilesConfig`, `FleetSpawnConfig`, `FleetSpawnBase`; `fleet_policy` on tool-batch requests (renamed `subagents_policy`); `PromiseSource::Run` and its arms in cancellation and source polling (`RunTerminalNotifyIntent` stays — bots and the execution workflow use it) |
| api | `FleetFeature` family; `FeaturesConfig.fleet` → `subagents`; `api-projection` fleet mapping; regenerated contract, OpenRPC, TS client, Configurator tools, web reference |
| store | `session_links` table, `SessionLinkRecord`, `UpsertSessionLink`, `ListSessionLinks`, `SessionLinkDirection`, the store trait methods and `store-pg` implementations |
| platform | the Fleet card in `session-config-editor.tsx`; the fleet entry in `profile-config-reference.ts` |
| docs | fleet claims in `README.md`, `docs/design.md`, `docs/landing-page.md`, `docs/multi-tenancy.md`; `docs/spec/03-fleet-idea.md` deleted (its ideas live here or in the reference study); the fleet-vs-bots review's status points here |
| tests | `run_fleet_*` live clients in `subagents_live.rs`, replaced by the sub-agent scenarios below |

Separable, recommended: the **mailbox**. The audit found exactly one
`SubmitMessage` caller — `deliver_message_for_fleet`. With `agent_send`
gone, `await { mailbox }`, `MessageBuffered` / `MessageConsumedByAwait` /
`MessagePromotedToRun` / `MessageCancelled`, `WakeReason::MailboxMessage`,
`AwaitOutcome::MailboxMessage`, `RunOrigin::Message`, `runs.messages`, and
the `SubmitMessage` command have no producer. Remove them in their own
slice. The A2A adapter note that mapped `input-required` onto the mailbox
can re-grow it as an opt-in await field if that adapter is ever built (P129
said the same).

### 10. Contract and documentation

- `cargo run -p api --bin export-schema` and
  `cargo run -p temporal-workflow --bin export-workflow-contract` after the
  DTO changes; `npm run check` regenerates every TypeScript consumer.
- `README.md` "Managed sessions and workflow-backed tools" gains a
  sub-agents bullet; `docs/design.md` replaces the fleet paragraph;
  `AGENTS.md` architecture rules gain the two-tier rule (durable
  orchestration in bots, attached delegation in core, one-shot children,
  origin is provenance not ownership, no parent-side delegation transport).
- `docs/spec/05-subagents-reference-study.md` stays as the comparison it is;
  its "Lightspeed Design Position" gets a pointer here.

## Migration (greenfield)

`crates/store-pg/migrations/001_core.sql` creates `sessions` with
`origin_json jsonb`, `origin_root_session_id text`, and
`origin_parent_session_id text`, plus the shape CHECK and two partial indexes;
the retired `session_links` table is absent. Existing dev databases are reset
(`./dev.sh reset`); nothing is backfilled.

## Slices

1. **Store + API lineage** (½ day): `001_core` baseline, `SessionRecord.origin`,
   `CreateSession` with origin and the transactional root reservation,
   `SessionOrigin` on both views, `session/list` filters, link traits
   deleted, contract regenerated.
2. **Grant** (½ day): `SubagentsFeature` in `harness` + `api` replacing the
   fleet family; admission validation of `agents[]`; `subagents_policy` on
   the batch request; `api-projection`; Configurator/TS/web regeneration;
   the web config-editor card.
3. **Execution workflow + bindings** (2 days): `SubagentExecutionWorkflow`,
   recipe registration in the worker, prepare/resolve/close activities from
   `FleetService::spawn`, admission-time execution context and validation
   in `session_tools.rs`, the two system bindings and their toolset
   exposure, the result envelope. Delete `tools/src/fleet`, the fleet
   gateway entry points, `PromiseSource::Run`, and the dedup paths. Live
   scenarios below.
4. **Catalog** (½ day): `SubagentCatalog` context kind, rendering, the
   projection-refresh and idle-read refresh hooks, removal on revoke.
5. **`inherit`** (½ day): the profile variant, applier resolution in the
   prepare activity, rejection outside a spawn, web profile reference.
6. **Docs and sweep** (½ day): README/design/landing/multi-tenancy, delete
   `spec/03`, fleet-vs-bots status, roadmap, AGENTS.md rule.
7. **Mailbox removal** (1 day, separable): the engine and workflow
   subtraction listed in §9, with the P129 live coverage re-run.

1 and 2 can land together; 3 depends on both; 4, 5, 6 are independent of
each other after 3; 7 is independent throughout.

## Tests

- **Engine**: no new engine behaviour; the fleet feature tests are deleted
  and the joined-batch tests already cover multi-call park/resume.
- **Store**: origin round-trip; reservation rejects at each limit; the
  deterministic child id makes a retried create idempotent; `session/list`
  filters by root and parent.
- **Admission** (`session_tools`): unlisted agent, absent grant, missing
  profile, and over-deadline all fail at admission with typed errors and no
  start request.
- **Execution workflow** (unit, in `temporal-workflow`): prepare failure
  resolves `Failed` and exits; run terminal → resolution → close; holder
  cancellation and Temporal cancellation both close the child; deadline
  resolves `Failed { deadline }`; the recovery query answers after a terminal.
- **Live** (`temporal_live`, replacing `run_fleet_*`): `agent_run` single
  child result inline; three `agent_run` calls in one turn resume with three
  results; `agent_spawn` + `await any` + `cancel` of the rest closes those
  children; parent run cancelled while parked closes all children; a
  nested child hits `maxDepth` and the parent sees the typed failure;
  `maxDescendants` exhaustion; `inherit` activates the parent's provisioned
  environment and leaves it open when the child closes; catalog reflects a
  profile description edit at the next run.
- **Platform**: the sessions UI renders a tree; the bots integration suite
  is unchanged (no controller change); `npm run check` and
  `check:identity` green.

## Non-goals

- Follow-up runs into an existing child (run-owned lifetime) — reserved by
  `parentRunId`, not built.
- Clone/fork as a spawn base. `session/clone` and forks stay as store and
  API capabilities for debugging and branching.
- Child→parent messaging mid-run, an agent graph surface, or an agent
  type/manifest system (`docs/spec/03` is deleted, not revived).
- Inline (model-authored) profiles as spawn targets. Operator-authored
  inline entries in `agents[]` are a possible later addition.
- Per-child environment isolation.
- Tree-level token budgets; run limits remain per session (`limits`) and
  per bot (`runsPerDay`, spent by bot runs and by the sub-agent sessions
  they delegate).
- An A2A remote target. When that adapter lands it is another recipe behind
  the same two tools, not a new tool family.
