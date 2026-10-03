# P140 — Bot Environments and Bot Lifecycle

**Status**

- **Implemented 2026-08-27 (v2), all four slices**; reviewed by Lukas
  before implementation (two nits folded in: per-session `provision` stays a
  valid bot choice; the Environments page needed an idle-policy editor). See
  the implementation notes under Slices. Still open: a live dogfood of bot
  close/delete and of the idle-policy modal on the dev stack.
- Decisions taken with Lukas, in order:
  1. A bot's environment is, most of the time, an **`existing`**
     environment. Profiles keep exactly the intents they have today —
     `existing`, per-session `provision`, `inherit` — and nothing is added
     to the core's environment model. One environment per bot, one per team
     of bots, or a box an interactive session joins are all the same
     configuration: create the environment once, point profiles at its id.
     Per-session `provision` stays available to bots for the sandbox-per-
     event case (one VM per routed or event session, closed with it); it
     already works and is the exception, not the default.
  1a. **Existing environments need an idle-policy editor.** The Environments
     page only displays the policy today; the `environments/idle-policy/put`
     route exists but has no UI. A per-environment modal (and the same
     fields in the create dialog) is part of this proposal.
  2. **Power needs nothing new.** An idle policy is a property of the
     environment record ([P126](p126-environment-power-and-idle-policy.md)),
     applied by the reaper to every `ready` provisioned-source environment
     whoever references it, and wake-on-use fires for any session that
     activates a powered-down one. An `existing` bot box therefore already
     pauses, suspends, stops, and wakes. Only *external* environments (a
     daemon you bring) have no power control.
  3. Keep one core hardening from v1: **use cancels a pending power-down**
     (the reaper-vs-first-use race, §3).
  4. Bots get a lifecycle that mirrors sessions: `disabled` stays the
     reversible pause; **`close`** is terminal (releases sessions and
     schedules, keeps history); **`delete`** erases and frees the name, and
     closes first if needed. Close cancels in-flight runs immediately; a bot
     never closes or deletes an environment.
  5. Everything else is convenience on the platform: an environment card on
     the bot page, exec pollers defaulting to the bot's environment, editor
     and `profiles check` hints.
- **v1, same day, reverted.** The first P140 added
  `provision { scope: controller }` — one environment keyed off the managed
  session's lifecycle controller, request id
  `controller:<sha256(scope)>:g<n>`, generations replacing a closed one — and
  was implemented end to end (five slices, `bot-envs` commits `37fb5652`
  and `66b2b9e9`). It was thrown away (`git reset --hard 5033e523`) when
  "what if different bots share an environment?" showed the lifecycle
  controller is the wrong grouping axis: it gives one box per bot by
  construction, and the fix on that path (a third `shared` scope with a
  key) was more model for something `existing` already does. Nothing of v1
  is kept except the resolver hardening and the bot lifecycle design.
- Builds on [P125](p125-profile-provisioned-environments.md) (intents,
  `originSession`, `await_environment_ready`), P126 (power intent, idle
  reaper, wake-on-use), [P130](p130-bots.md) (controller, routed sessions,
  rotation, retention), [P134](p134-subagents.md) (`inherit`, lineage),
  [P135](p135-bot-federation.md) (directory, `bot_emit` refusals), and
  [P139](p139-channels-as-bot-triggers.md) (chat triggers, never-expiring
  chat sessions).

## Why

A bot is a session factory: `resolveBotProfile` passes the profile's
`environment` intent through verbatim (`platform/bots/src/contracts/bots.ts`)
into every `session/managed/start` the controller makes
(`platform/bots/src/activities/lightspeed.ts`) — the main session and its
rotated generations, every routed session, every chat conversation. Each is a
separate core session that applies the profile on its own.

With per-session `provision` that is one VM per session whose state dies
with the session: main-session rotation discards it, the routed-session
retention sweep closes one per key, a chat conversation (never expires) pins
one per conversation. That is the right shape for a bot whose job is a
disposable sandbox per event, and the wrong default for the common bot,
which wants one durable box. Exec pollers are stranded on it, and the bot
page today tells the operator to "point the profile at an existing
environment" as if that were a workaround. It is not — it is the design.
What the codebase lacks is treating it as such: nothing shows a bot's
environment next to the bot, exec pollers have to be handed an id, the
Environments page can display an idle policy but not set one on an existing
environment, the editor and CLI give no hint that the policy lives there,
and there is no way to retire or delete a bot at all
(`platform/backend/src/routes/bots.ts` has create and patch only;
`enabled: false` pauses).

## Today

- `ProfileEnvironment` (`crates/api/src/profiles.rs`): `existing`, `inherit`
  (sub-agents), `provision { providerId, templateId, retention, idlePolicy?,
  credentials? }` with `requestId = "session:" + sessionId` and an optional
  `closeWithSession` trigger on `originSession`.
- The applier (`crates/temporal-runtime/src/gateway/service/profiles.rs`)
  activates an `existing` environment through the same status-aware
  admission as `session/environments/activate`: `provisioning`/`booting`
  and powered-down environments are admitted as intent (P125/P126),
  `closing`/`closed`/`failed` are rejected with a typed error.
- Idle policy and power intent live on the environment record
  (`crates/environments/src/lib.rs`, `EnvironmentRecord.{desired_power,
  idle_policy}`); set at `environments/create`, changed with
  `environments/idle-policy/put` and `environments/power/put`, and shown
  with Resume/Pause/Suspend/Stop on the Environments page. The reaper
  (`environment_power.rs`, `decide_idle_action`) reads the daemon's
  `env/idle` report — `{ idleForMs, runningProcesses, runningJobs }` on one
  monotonic clock bumped by every data-plane request — and never consults
  sessions. Wake-on-use keys on `EnvironmentSource::Provisioned`
  (`environment_resolver.rs`, `wake_on_use_applies`), not on who
  provisioned it.
- The bot controller (`platform/bots/src/workflows/bot-controller.ts`)
  tracks its sessions in memory (main generation, `extraSessions` capped at
  `EXTRA_SESSION_CAP = 200`, per-key generations), closes routed sessions by
  TTL, and closes descendants first through `closeBotSession` (non-force).
  Admission re-reads the bot row and refuses when `enabled` is false
  (`platform/bots/src/admission.ts`); `wakeBotController` is a
  `signalWithStart` (`platform/bots/src/events.ts`).
- `session/close { force }` cancels the active run and drops queued runs;
  `session/delete` on an open session force-closes first, then removes the
  record (`crates/temporal-runtime/src/gateway/service/mod.rs`).

## Design — Part A: a bot's environment is an `existing` environment

### 1. The configuration

```text
1. Environments page (or `environments/create`): one box from a provider
   template, with an idle policy — e.g. pause after 10 min, stop after 6 h,
   no close stage. Bind credentials on it if the bot needs any.
2. Profile: environment: { type: "existing", environmentId: "<that id>" },
   config granting features.environments (jobs: true for job tools).
3. Bot: profileId = that profile.
```

- One box per bot: one environment, one profile, one bot. One box for a
  team of bots: one environment, N profiles (or one shared profile), N
  bots. An operator who wants to poke at the box interactively starts a
  session with the same profile, or activates the environment by id.
- Sub-agents that should work on the bot's box use a profile with
  `environment: { type: "inherit" }` and the environments grant; children
  that should be isolated use per-session `provision`. Unchanged from P134.
- A bot whose work is a sandbox per event keeps per-session `provision`
  (default `closeWithSession`): one VM per routed or event session, gone
  with it, exactly as today. Nothing here changes that path; the bot page's
  warning about exec pollers stays because it is true for it.
- Nothing on the bot record references an environment. The profile is the
  only link, exactly as for any session, and `resolveBotProfile` keeps
  passing it through untouched.

Rejected (v1): a `provision { scope: controller }` that auto-creates one
environment per bot with generations. It cannot express a box shared by
several bots, needs a scope column, a request-id scheme, generation
lookup, a pre-start rejection for plain sessions, and a second creation
path for exec pollers — all to save one `environments/create`. Also
rejected on the same grounds: a `shared` scope keyed by name, a
caller-supplied scope key on `session/managed/start`, a lease/ref-count that
closes a box when its last session leaves, and a bot record in the core.

### 2. Power: nothing to add, and why it is safe with many sessions

"Safe to pause" is, and stays, `runningProcesses == 0 && runningJobs == 0 &&
idleForMs >= threshold` from the daemon's report:

- Every session's `fs/*`, `process/*`, `job/*` call — main, routed, chat,
  sub-agent, another bot's session, an operator shell, an exec poll job —
  touches the same clock, so N sessions sharing a box need no coordination;
  the union is computed by construction. That is precisely why P126 chose
  the daemon over session-derived idleness.
- A background process started by any session pins the box awake. Right:
  freezing it is harmless, `stopped` would kill it.
- An LLM-only turn is not use. A session mid-run can have its box paused
  underneath it; its next environment tool call gets `NotReady`, the
  workflow waits in `await_environment_ready`, and the call is
  re-dispatched. Stage the policy accordingly: `pause` (freeze,
  milliseconds to resume) after minutes, `suspend`/`stop` after hours.
- Exec poll jobs count as activity and wake a sleeping box (P131's patch);
  a poll interval shorter than `pauseAfterMs` keeps it awake, longer cycles
  it — fine for freeze, wasteful for `stopped`.

Rejected: any session→environment index so the reaper could skip boxes
whose sessions have a running run. A chat session is effectively always
open and a run can be a two-hour LLM-only turn; the proxy would keep boxes
awake for nothing and is blind to non-session use.

### 3. Use cancels a pending power-down (core, kept from v1)

The one race in P126: the reaper reads "quiescent" and sets
`desiredPower: paused`; a session starts work before the reconciler
converges; the reconciler freezes the box under the call. P126 accepted it
(the P114 activity deadline fires, the retry sees `paused` and wakes it).
This narrows it: in `resolve_for_connection`, a `ready` environment whose
`desiredPower != running` gets one conditional write back to `running`
before the call proceeds — the same shape as the wake branch, still
session-blind, independent of how the environment is referenced. Unit test
in `environment_resolver.rs`; no wire change.

### 4. When the box goes away

An `existing` environment that is `closing`/`closed`/`failed` fails the next
profile apply with a typed error, so a new bot session cannot start and the
controller reports `degraded`. There is no automatic replacement — no
generations — and that is the accepted trade:

- **Rule: no `closeAfterMs` on a bot box.** `stop` is the deepest stage; a
  stopped environment costs only disk and comes back on the next use. A
  box disappears only when an operator closes it.
- When an operator does replace a box: create the new one, edit the
  profile's `environmentId`; the controller re-applies the profile on the
  next revision change, and every bot session moves over (a session's
  active environment changes on apply).
- The bot page shows the environment's status prominently, so a closed
  box is visible next to the bot, not buried in a session error.

### 5. Platform conveniences

- **Idle policy editor on the Environments page.** Today
  `EnvironmentsPage.tsx` renders the policy read-only and the create dialog
  has no policy fields, while `PUT /api/v1/universes/:id/environments/:id/
  idle-policy` (→ `environments/idle-policy/put`) already exists. Add an
  "Idle policy" modal on every provisioned environment row — the four
  stages in minutes with the monotone check, "clear" as an explicit
  action — and the same fields in the create dialog. The stage inputs move
  out of `profile-environment-editor.tsx` (`IdlePolicyFields`) into a shared
  `components/environment/idle-policy-fields.tsx` so the profile editor,
  the create dialog, and the modal render one thing. External environments
  and closed ones do not offer it (the core rejects it for them).
- **Bot page environment card**: for a profile with `existing`, show the
  environment (status, desired power with the converging arrow, idle
  policy, template) with the Environments page's Resume/Pause/Suspend/Stop
  controls (moved into a shared `components/environment/power-controls.tsx`)
  and a link to the page. Amber hint when the environment has no idle
  policy ("this box never sleeps") or is external ("no power control").
  Red when it is closed/failed. The existing per-session warning stays for
  `provision` profiles.
- **Exec pollers** (P131): `environmentId` becomes optional on the poll
  spec, `bot_trigger_put`, and the web form; at fire time it resolves to the
  bot profile's `existing` environment, and a bot whose profile is not
  `existing` gets a clear configuration error (not a transient failure).
  The "Command poll" card is enabled exactly when the profile is `existing`
  (as today).
- **Profile editor**: the `existing` mode description says where the idle
  policy lives and links to the Environments page; the environment option
  labels carry the environment's power/idle state. The `provision` label
  says "for the session".
- **`profiles check`**: for `existing`, warn when the environment has no
  idle policy or is external; error when it is closed (today's typed
  read error already covers "missing").

## Design — Part B: bot lifecycle

### 6. Three states, two of which exist

| state | reversible | live resources | history | name |
| --- | --- | --- | --- | --- |
| **disabled** (exists) | yes | kept: sessions and chat context stay; the environment sleeps by idle policy | kept | reserved |
| **closed** (new) | no | released: sessions closed, schedules dropped, triggers disabled | readable | reserved |
| **deleted** (new) | — | — | erased | freed |

Disable cannot double as close: it must preserve chat conversations and
routed-session context for re-enable. Close earns its own state because it
is cheap (delete needs the same teardown; the state is one `closedAt`
column plus "reject `enabled: true` while closed") and because "this bot is
done, keep what it did" is a real operator request — the same reason
`session/close` exists next to `session/delete`. Like `session/delete`,
`delete` on an open bot closes first, so it stays one call when nobody
cares about the distinction.

A bot never closes or deletes an environment: its box is an `existing`
universe resource other sessions or bots may share; a per-session
provisioned environment closes with its session through the core.

### 7. Close

`POST /api/v1/universes/:id/bots/:botId/close`. The server writes
`closedAt` (and `enabled = false`) **first**, so every later step is
idempotent and retry-safe, then:

1. **Server**: set every trigger `enabled = false` with `disabledReason:
   "bot_closed"` and reconcile schedules (paused; deleted at delete time —
   chat triggers stop matching on their account, other bots' triggers on
   the same account are untouched). Admission refuses on `closedAt` the
   same way it refuses on `enabled`: webhooks answer `410`, the manual
   event route `410`, `bot_emit` to a closed bot returns a typed
   `bot_closed` refusal (distinct from `bot_disabled`, so a sending model
   stops retrying), schedule and poll fires exit on the disabled trigger,
   the `bot:directory` catalog omits it (close sets `enabled = false`).
   The resurrection guard matters: `wakeBotController` is a
   `signalWithStart`, so `storeBotEvent` must refuse on the row before
   anything is stored — otherwise a late webhook would start a fresh
   controller for a bot that no longer exists.
2. **Controller**: `BotStartV1` gains `closed?: boolean`; the config signal
   (or a `signalWithStart`, if the workflow is gone — the teardown then runs
   in a fresh run, which is exactly what restart safety wants) makes the
   controller stop dispatching and run the teardown as activities:
   - buffered, pending, and in-flight events get outcome `archived`,
     detail `bot_closed`; active lanes are not waited for — they lose their
     session underneath and settle on their own;
   - every session it knows — main generations `1..N` and every tracked
     routed session — is closed with `force: true`, descendants first
     inside the activity (`closeBotSession` grows a `force` flag; an
     already-closed session counts as done);
   - the controller records its final session set on the bot row
     (`bots.closed_sessions jsonb`, union with any earlier attempt) — the
     authority on generations is the controller, and after it returns
     nothing else knows them;
   - the workflow **returns** instead of continue-as-new;
     `controllerStatus` reports `closing` / `closed`. The server's close
     route awaits the run result with a bounded timeout (30 s) and answers
     `{ bot, completed }`; a timeout still leaves `closedAt` set and the
     next close call re-signals.
3. `PATCH { enabled: true }` on a closed bot is `409`; label patches
   (display name, description) stay allowed and do not signal the
   controller.

Pre-existing gap fixed on the way: `ensureRoutedSession` evicts sessions
beyond `EXTRA_SESSION_CAP` from memory without closing them, so they leak
out of the retention sweep and out of any teardown. Eviction closes the
session (non-force; a busy one stays tracked until the next sweep) and
bumps its generation.

### 8. Delete

`DELETE /api/v1/universes/:id/bots/:botId`. If the bot is open, run the
close procedure first and wait for it (`409` while the teardown has not
completed). Then:

1. `session/delete` every id in `closed_sessions` (`not_found` is fine — a
   rotation that was never ensured). Sessions must go: their ids derive
   from the immutable bot name, so a re-created bot would collide with its
   predecessor's closed sessions at `session/managed/start`. Sub-agent
   descendants were closed at close time; their ids (`agent_<digest>`) do
   not derive from the bot name, and their records stay as history.
2. Delete each trigger through `deleteTrigger` (drops its Temporal
   Schedule), then the `bots` row; `bot_events` and `channel_pairings`
   follow by cascade. The name is free.
3. Environments are untouched. The completed controller workflow id is
   reusable under Temporal's default reuse policy.

### 9. Rules

- A bot's environment is the profile's `existing` environment: a universe
  resource nothing bot-related creates, closes, or deletes.
- The reaper never consults sessions. Use — any data-plane request from
  anyone — is the only activity, and use cancels a pending power-down.
- No `closeAfterMs` on a bot box; `stop` is the deepest stage.
- `disabled` is reversible and keeps sessions; `closed` is terminal and
  releases them; `deleted` erases. Nothing revives a closed bot.
- Close writes `closedAt` before any teardown step; every step is
  idempotent; admission refuses on the row, never on controller state.
- Bot `delete` closes first.

## What is deliberately not built

- Any new environment intent or scope (controller, shared, named), request
  id scheme, generations, or provenance column (v1, reverted).
- Auto-creation of a bot's environment from the bot record.
- A lease/ref-count that closes a box when its last session leaves.
- Session-aware power decisions or a session→environment index.
- A grace period or drain before close cancels runs (default: immediate;
  revisit if a bot's runs turn out to be long transactions worth
  finishing).
- `bots/reopen`; deleting environments or sub-agent history with a bot.
- Operator CLI commands for bots — the CLI has none today and this adds
  none; routes and the web UI are the surface.

## Wire and record changes

Core `api`: **none.** The resolver change (§3) alters behavior, not the
contract.

Platform (`platform/db`, folded into the pre-release `0002_bots` baseline
with the snapshot-patch trick from P135): `bots.closed_at timestamptz`,
`bots.closed_sessions jsonb`; `bot_triggers.disabled_reason` takes
`bot_closed`. `BotView` gains `closedAt` and `closedSessions`.
`BotStartV1.closed?: boolean`. `BotSnapshot.controllerStatus` gains
`closing` / `closed`. Poll spec `source.environmentId` becomes optional.

## Runtime and platform changes

- `crates/temporal-runtime/src/environment_resolver.rs`: `ready` +
  `desiredPower != running` → conditional write back to `running`.
- `crates/cli` `profiles check`: `existing` idle-policy / external / closed
  hints.
- `platform/bots`: admission refusal code `bot_closed` (`resolveInbox`,
  `storeBotEvent`); controller `closed` handling (`teardown()`, `wake()`
  fires on it even under an active lane, return instead of continue-as-new,
  eviction closes); `closeBotSession { force }`; `recordBotClosed`
  activity; poll fire resolves a missing `environmentId` from the bot
  profile; `bot_trigger_put` and its schema description accept the
  omission.
- `platform/backend`: `bots/:botId/close`, `DELETE bots/:botId`, `409` on
  enabling a closed bot, `410` on webhook ingest and manual events for a
  closed bot; `signalBotConfig` carries `closed`.
- `platform/web`: Environments page idle-policy modal and create-dialog
  fields (shared `idle-policy-fields.tsx`); bot page environment card;
  shared power controls; Close
  and Delete in the bot settings dialog (Close confirms irreversibility;
  Delete explains close-first); "Closed" on the bots list and a closed note
  on the detail page; trigger cards show "bot closed"; poll form with an
  optional environment; profile editor copy.

## Slices

1. [x] **Core hardening** — resolver "use cancels a pending power-down" with a
   unit test. Tiny; no contract change.
2. [x] **Environment surfaces** — idle-policy editor modal and create-dialog
   fields on the Environments page, bot page environment card (shared
   power controls), exec poll default, editor copy, `profiles check` hints.
3. [x] **Bot close and delete** — platform columns, admission guards,
   controller teardown, routes, web dialogs, integration scenarios.
4. [x] **Docs** — `AGENTS.md` (rules in §9), `docs/spec/04-environments.md`
   (a paragraph on shared use and the power-down cancel), P130 status note,
   P131 §3 note (exec pollers default to the bot's environment). `README.md`
   needed no change: its profiles bullet already describes `existing` and
   per-session `provision`.

### Implementation notes (2026-08-27)

- **Core**: `resolve_for_connection` on a `ready` environment whose
  `desiredPower != running` writes the intent back to `running` and
  proceeds (`environment_resolver.rs`, test
  `use_cancels_a_pending_power_down`). No `api` change; the contract
  artifacts are untouched.
- **Environments page**: `components/environment/power-controls.tsx`
  (power helpers and Resume/Pause/Suspend/Stop, moved out of the page),
  `components/environment/idle-policy-fields.tsx` (the four stages plus
  `idlePolicyIsMonotone`, moved out of the profile editor and shared by
  the profile editor, the create dialog, and the new modal), and
  `components/environment/idle-policy-dialog.tsx` (`PUT
  …/environments/:id/idle-policy`, explicit Clear, amber hint when empty,
  warning on a close stage). The environment card gets an "Idle policy…"
  button on open provisioned environments; the create dialog gets the stage
  fields (`idlePolicy` was already forwarded by the server route) and a
  hint that a shared box should at least pause.
- **Bot page**: `components/bot/environment-card.tsx` for `existing`
  profiles — status, power convergence, idle policy, power controls, the
  idle-policy modal, and red/amber notes for a closed, policy-less, or
  external environment. `DetailSection` moved to `bot/status.tsx`. The
  per-session `provision` warning stays and now says "a sandbox per event".
- **Exec pollers**: `environmentId` optional end to end (zod schema, row
  type, `bot_trigger_put` mapping and tool descriptions, web form and
  validation); `resolveBotProfileEnvironment` in `activities/poll.ts`
  reads the bot's profile at fire time and uses its `existing` id, failing
  with a configuration error for any other intent.
- **CLI `profiles check`**: an `existing` environment that is
  closing/closed/failed is an error; a provisioned one without an idle
  policy, or an external one, is a warning.
- **Bot lifecycle**: exactly the v1 build minus any environment step —
  `bots.closed_at` / `bots.closed_sessions` folded into the `0002_bots`
  baseline (snapshot patched, no-op `drizzle-kit generate`,
  `test:migrations` asserts both); `bot_closed` refusal from `resolveInbox`
  and `storeBotEvent`; `410` on webhook ingest and manual events;
  controller `teardown()` (archive → force-close every known session →
  `recordBotClosed` → return; `closing`/`closed` status; `wake()` fires on
  it under an active lane; `EXTRA_SESSION_CAP` eviction closes the
  session); `POST …/close` (row first, triggers `bot_closed`, schedules
  paused, signal, ≤ 30 s wait, `completed`); `DELETE …` (close first, `409`
  while pending, `session/delete` recorded sessions, `deleteTrigger` per
  trigger, row cascade); `PATCH` labels-only on a closed bot; web
  Lifecycle block with Close/Delete confirms, "Closed" on list/detail,
  "bot closed" on trigger cards.
- **Verified**: `cargo test -p temporal-runtime --lib environment_resolver`,
  `cargo clippy -p temporal-runtime -p cli --all-targets`, `npm run
  typecheck/test/build`, `check:identity`, `test:migrations` against a
  scratch platform database, bots Temporal integration 18/18 including
  the new close scenario (pending event archived `bot_closed`, forced
  session close, sessions recorded, `COMPLETED`, history replays, second
  signal-with-start tears down again without creating a session).
- **Deploy note**: the platform `0002_bots` baseline changed → existing dev
  databases need `./dev.sh reset`. The core `005_environments` baseline is
  untouched (v1's column is gone with v1).

## Tests

Core unit: resolver `ready` + pending power-down flips desired back to
`running` and proceeds; the powered-down wake branch is unchanged.

Bots unit: `resolveInbox` refuses `bot_closed` before `bot_disabled`;
`bot_trigger_put` maps an exec poll without `environmentId`; the poll
activity's environment resolution picks the profile's `existing` id and
fails clearly otherwise.

Bots integration (`BOTS_TEMPORAL_INTEGRATION=1`, `npm run
test:integration:bots`):

- close: a disabled bot with a pending event receives `closed` → the event
  ends `archived` / `bot_closed`, the main session is closed with
  `force: true`, `closed_sessions` is recorded, the workflow `COMPLETED`,
  history replays; a second signal-with-start with `closed` tears down
  again without creating a session;
- eviction beyond `EXTRA_SESSION_CAP` closes the evicted session.

Platform: `test:migrations` asserts `closed_at` and `closed_sessions` on
fresh and upgrade paths. Consumers: `npm run check`.

Live (dev stack, manual): create a box, set its idle policy from the new
modal, point a bot at it, watch the bot page card pause it after the
threshold and wake it on the next event; close the bot and confirm the box
stays; delete the bot and re-create it under the same name.

## Open questions

1. Whether `close` should offer an opt-in drain (`{ drainMs }`) for bots
   whose runs perform multi-step external writes. Default is immediate
   cancel.
2. Whether the Environments page's create dialog should suggest a default
   idle policy (P126 open question 2 again). For now the bot page and
   `profiles check` warn.

## Deferred

- Pre-wake on `runs/start` for a powered-down active environment (an
  optimization, not safety).
- Deleting sub-agent descendants' records with the bot.
- `bots/reopen`.
