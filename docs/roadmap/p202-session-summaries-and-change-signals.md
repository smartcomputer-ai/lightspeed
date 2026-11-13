# P202 — Session and bot summaries with a change signal

**Status:** Implemented and functionally validated locally, 2026-11-13.
PostgreSQL `LISTEN`/`NOTIFY` is the first transport. Session activity and
persistent display details share `session_summary`; `sessions` keeps the
catalog, lifecycle and log head. Bot controller status lives directly on
`bots`. Existing sessions do not require historical summary backfill.
Step-by-step phases remain deferred.

Provides the server data asked for in
[#149](https://github.com/smartcomputer-ai/lightspeed/issues/149) and removes
the reader polling measured in
[#121](https://github.com/smartcomputer-ai/lightspeed/issues/121). The client
stream that pushes these changes to browsers is
[P203](p203-universe-watch-stream.md) and builds on this document.

## Goal

1. A list row for a session or a bot carries what a list UI shows: what it is
   doing and why, how its last run ended, its latest message, and what is
   attached. No follow-up read per row.
2. Any process that commits a change tells every gateway process, so the
   gateway wakes promptly instead of relying on its polling interval. Recovery
   timers remain in place.

The two parts change the same store functions, and the row revision added for
the first is what readers compare after the second wakes them. They are one
piece of work. The signal lands first because it needs no migration and no
contract change.

## Starting point

- **Nothing is pushed.** `session/events/read` with `waitMs` re-reads
  PostgreSQL every 250 ms or less (`read_session_events` in
  `crates/temporal-runtime/src/gateway/service/mod.rs`). #121 measured 12 SQL
  statements per idle reader per second.
- **Gateway waits poll.** About fifteen waits in
  `crates/temporal-runtime/src/gateway/service/` (`wait_for_run_accepted`,
  `wait_for_steering_accepted`, `wait_for_approval_decision`,
  `wait_for_closed_session`, `wait_for_cancelled_run`, the context waits)
  sleep `DEFAULT_POLL_INTERVAL` (500 ms) between a Temporal status query and
  a store read. #121 attributed about 346 ms of low-load run-start latency to
  this.
- **Lists poll.** The web refetches the session list and bot roster every
  5 s and bot state every 3 s. Each bot-state read is a Temporal query to the
  controller workflow.
- **The append transaction is already a projection point.**
  `apply_lifecycle_projection` (`crates/harness/src/storage/session.rs`)
  folds committed entries into `SessionRecord`, and `append_inner`
  (`crates/store-pg/src/session.rs`) writes the result to `sessions` and
  `session_activity` in the same transaction as the events. Every appender
  goes through `SessionStore::append`: the workflow activity, the gateway,
  the reaper and sub-agent code.
- **Lists know little.** `SessionSummaryView` has lifecycle status and a
  three-value `activity` (idle, working, waiting). It has no failure, no
  message text, no attachments and no active environment. Rename, metadata
  and share do not move `updatedAtMs`, so a client cannot detect them.
- **Bot status lives in the workflow.** `bots/list` computes pending count,
  last event and activity in SQL (`list_bot_roster_for` in
  `crates/store-pg/src/bots.rs`). Controller status (degraded, budget
  exhausted, closing) exists only in `BotControllerCarry` and is read through
  a Temporal query.
- **Environment signals are process-local.** `EnvironmentSignals`
  (`crates/temporal-runtime/src/environments/signals.rs`) nudges the
  reconciler and revokes routes only inside one process. A nudge from an
  API-only gateway process reaches nothing; sweeps catch up later.

## Part 1: the change signal

### Contract

One small message, published after a change is committed:

```text
{ universe, kind, id, seq?, rev? }
kind = session | bot | environment
```

For a session, `seq` is the head sequence when the change was an append,
and `rev` is the row's new revision when the change altered what a list
shows. An append that only extends the log carries `seq` alone. Readers
following events act on any signal; readers showing lists act only on
signals with `rev`. With hundreds of sessions working at once, this keeps
list traffic proportional to visible changes instead of to appends.

Three rules make the signal safe to lose:

- **It carries no data.** A reader that is woken reads the store.
- **It may be lost or duplicated.** Every reader keeps a cursor or revision
  and a fallback timer, and stays correct with no signal at all.
- **It may be merged.** Several changes to one row may arrive as one signal.

### Transport

PostgreSQL `LISTEN`/`NOTIFY`, behind a small `ChangeBus` with a local-only
mode for tests. Every runtime
process already connects to PostgreSQL, so this adds no infrastructure and
no replica discovery.

**Publish after commit, never inside the writing transaction.** PostgreSQL
takes a cluster-wide lock when a transaction that issued `NOTIFY` commits,
which serializes those commits. The append path is the hottest write path
and #121 shows it is sensitive, so it must not notify in-transaction and
must not use a trigger. Instead:

- A store method that commits a change hands `{universe, kind, id, seq}` to
  a publisher owned by the store.
- One task per process collects signals for about 20 ms, drops duplicates,
  and sends them as one `pg_notify` on a single channel. A payload is
  limited to 8000 bytes, so a large batch is split.
- The notifying transaction contains only the notify and runs with
  `synchronous_commit = off`, so it never waits for a WAL flush.
- A failed publish is logged and counted. It never fails the write.

The cost of publishing after commit is that a process that dies between its
commit and its publish loses that signal. The fallback timer covers it.

### Payload

One channel, `lightspeed_changes`. Each notification is one JSON document
holding the batch:

```json
{
  "v": 1,
  "changes": [
    { "u": "<universe uuid>", "k": "session", "id": "<session id>", "seq": 412 },
    { "u": "<universe uuid>", "k": "session", "id": "<session id>", "seq": 98, "rev": 17 },
    { "u": "<universe uuid>", "k": "bot", "id": "<bot id>" },
    { "u": "<universe uuid>", "k": "environment", "id": "<environment id>" }
  ]
}
```

- **Identifiers only.** No message text, names, previews, status values or
  credentials ever travel on the channel. Any database role that can listen
  sees these payloads.
- **Size.** An entry is roughly 100 bytes, so one payload holds about 60 to
  70 changes. A batch above 7,500 bytes is sent as several notifications.
- **Volume.** A publishing process coalesces changes for up to 20 ms,
  merging the maximum `seq` and `rev` independently. Large batches split
  into several notifications; bounded queues limit memory during outages.
- **Versioning.** A receiver that cannot parse a payload, or sees an
  unknown `v`, wakes every waiter. Mixed versions during a rolling upgrade
  therefore stay correct.
- **Not stored.** PostgreSQL delivers a notification only to listeners
  connected at that moment, on the primary. Nothing is written to a table
  and nothing is replayed.

A process that both publishes and subscribes also delivers the signal
locally at once, without waiting for the batch.

### Subscribing

Each runtime process holds one listening connection
(`sqlx::postgres::PgListener`) shared by its roles. An in-process registry
keys row waiters by `(universe, kind, id)`; a broadcast feed serves broader
subscribers, which filter by universe and kind. When
the listener reconnects it wakes every waiter, since signals may have been
missed.

Waits follow one pattern so that no change can fall between the check and
the wait: subscribe, read the store, then wait for a signal or the fallback
timer.

**Deployment requirement.** `LISTEN` needs a direct connection or a
session-mode pooler. A transaction-mode pooler such as PgBouncer drops it.
The runtime already has this requirement: the CAS sweep leader and
migrations take session-level advisory locks (`CasSweepLeader` in
`crates/store-pg/src/cas_sweep.rs`). The number of listening connections is
one per runtime process, independent of the number of users. The listener
uses `PgListener::connect_with` on the deployment pool so that the pool’s
Azure workload-identity credential refresh applies on reconnect too.

### What changes for readers

- **Event long-poll.** `read_session_events` waits for a `session` signal
  for its session instead of sleeping 250 ms. Fallback timer: 5 s.
- **Gateway waits.** Each wait in `gateway/service/` wakes on the session's
  signal. They keep the 500 ms timer as the fallback, because an admission
  that is refused is reported through workflow status without an append and
  therefore produces no signal.
- **Environment signals.** Committed environment changes publish an
  `environment` signal, so API-only gateways reach reconcilers and route
  holders in other processes. A route holder rechecks committed state before
  revoking a route; an invalidation alone is not a revocation command.

## Part 2: summaries

### Session summary

The summary answers two questions for a session nobody is watching: does
it need a person, and is it stuck. Which step a run is on belongs to the
session page, which follows events.

Extend the existing projection. It stays one pure function over committed
entries, shared by the in-memory and PostgreSQL stores, and it can be
rebuilt by replaying the log. The session read remains authoritative; the
summary is a display hint.

| Wire field (proposed) | Derived from | Notes |
| --- | --- | --- |
| `revision` | every change to what a list shows | Bumped when any field in this table, the lifecycle status, name, metadata, retention or access changes. An append that changes none of them moves `updatedAtMs` and leaves `revision` alone. |
| `activity` | unchanged | idle, working, waiting. Waiting keeps its meaning: an approval is pending. |
| `activityPhase` | `tool.batch_parked` and `tool.batch_unparked` | `awaiting` while the active run is parked on promises (a sub-agent, a timer, external work). Absent otherwise. Today such a run reads as working for as long as it is parked. |
| `activitySinceMs` | `session_summary.since_ms` | When the current activity or phase began. |
| `lastRun` | run terminal events | `{ runId, status, failureKind?, finishedAtMs }`. A failed last run is the list's error state. Today a failed run and a finished one both read as idle. |
| `executionFailure` | recorded by the reaper and by `session/read`; see [Stopped execution](#stopped-execution) | Same values as on the session read. |
| `lastMessagePreview`, `lastMessageRole`, `lastMessageAtMs`, `lastMessageOrigin` | `contextEntriesApplied` entries of kind message, role user or assistant | One plain line of at most 200 characters, reduced as the web's reference implementation does. |
| `activeEnvironmentId` | `environment.active_set` and `active_cleared` | Equals the session read's value. |
| `attachments` | `lifecycle.opened` and `config_changed` | `{ workspaces, environments, mcpServers }`, ids only. |

`lastMessagePreview` and `activeEnvironmentId` use the names the web already
reads. Their exact behaviour is specified in
`platform/web/docs/roadmap/u100-missing-server-implementation.md`, which is
on the `uiux/general-improvements` branch at the time of writing.

Event kinds in this document omit their `lightspeed.core.` prefix.

`activityPhase` is an open enumeration with one value for now. It is set by
`tool.batch_parked` and cleared by `tool.batch_unparked`, a run terminal
event or `lifecycle.closed`. An approval park still takes precedence in
`activity`. Parking and unparking are rare, so this adds almost no writes.

The last message is the latest context entry of kind message with role
user or assistant:

- **Counts:** run input, steering input, and every assistant message,
  including the text a model writes between tool calls in a run that is
  still going. A message appended through a context edit counts too.
- **Does not count:** reasoning, tool calls and tool results, and entries
  the runtime inserts itself, such as a compaction summary (source
  `Runtime`).

An assistant message is recorded when the model's turn completes, so the
preview is always a whole message and never partial output. It changes at
most once per turn, not per tool call. It is the latest eligible message, not a guaranteed final answer:
a completed run can end without a new assistant message.

**Who sent it** is told by two fields, both copied from the entry:

| `lastMessageRole` | `lastMessageOrigin` | Sender |
| --- | --- | --- |
| `assistant` | absent | The model. |
| `user` | `user:<platform user id>` | A person, through the Platform. The web compares the id with the signed-in user and resolves other names through the members list, as in [P204](p204-transcript-authors-and-times.md). |
| `user` | `event` | A bot delivery: a generated event prompt. A list can show it differently. |
| `user` | anything else, or absent | Another API client, or a parent agent's task in a sub-agent session. |

`origin` is display metadata. The Platform derives it from the signed-in
user and ignores a value in the request, so it is reliable for web input. A
direct API client can set any string. The authenticated requester is
`requestedBy` on the admitting event; the summary does not join it, because
that would make the projection carry state across events.

**What is stored.** The summary row holds the reduced line itself, at most
200 characters of plain text, so a list renders without a blob read per
row. It never holds the whole message. The whole message stays where it is
today: a blob in CAS, referenced from the event. The row is deleted with
its session.

**Where the text comes from.** The two roles differ today:

- **Assistant messages** carry the first 256 characters of their text as
  the entry's `preview`, inline in the event. Their content blob is
  provider-native JSON. The projection uses the `preview`.
- **User text messages** carry only the content reference; `preview` is
  empty (`user_message_input` in
  `crates/temporal-runtime/src/gateway/service/input.rs`). The store reads a
  bounded prefix (proposed: 4 KiB) of the blob inside the append
  transaction, and only when the blob is stored inline in PostgreSQL
  (`cas_blobs.inline_bytes`, used up to 64 KiB) and is text. That is one
  primary-key read per user message. A larger blob lives in object storage
  and is not read; the previous preview stays.

The store resolves the text and hands it to the projection, which stays a
pure function. A message that is empty after reduction also leaves the
previous preview in place.

`attachments` requires the projection to decode the session config in the
`lifecycle.opened` and `config_changed` events, which ties it to the config
schema. Those events are rare. Keep the decoded shape to ids per kind so a
config change seldom touches the projection.

### Stopped execution

When a session's workflow execution dies during a run, the list shows
"working" indefinitely. Only `session/read` asks Temporal whether the
execution is still there, and it sets `executionFailure` on its own
response without storing it.

The reaper already finds this case: every five minutes it describes the
execution of each open session that has an active or queued run, and
reports one that has stopped (`reap_session` in
`crates/temporal-runtime/src/worker/reaper.rs`). Record the result instead
of only logging it:

- The reaper writes `executionFailure` and the time to the summary row.
- `session/read` writes it too when it detects one that is not yet
  recorded, so opening the session corrects the list at once.
- The projection clears it when a run starts, a run ends or the session
  closes. Other appends, such as the reaper's own promise repairs, leave it.
- The write bumps `revision` and publishes a `session` signal. A failure
  observation is conditional on the revision read before querying Temporal.
  A confirmed restart clears the failure and advances the revision even if
  no failure was recorded, preventing an older observation from restoring it.

It is store metadata like the display name, not an event. An idle session
whose execution has stopped is still reported only by `session/read`,
because the reaper does not check sessions without a run.

### Storage

- `sessions.revision bigint` advances when list-visible state changes. Log-only
  appends advance `head_seq` and `updated_at_ms` without changing revision.
  Access and retention changes also invalidate descendants that inherit them.
- `session_summary`, keyed by `(universe_id, session_id)`, holds activity,
  phase and `since_ms` in columns and the remaining display details in JSON.
  It replaces `session_activity` and persists while idle. Idle has a null
  phase and activity start time. Bot queries filter activity explicitly;
  a partial index covers non-idle summaries.
- A summary is written only when its projected fields change, in the same
  transaction as the events. Appends whose kinds cannot affect the projection
  skip reading the summary. Lists join one summary row without replaying history.
  Mutators lock the catalog first, then read the summary in a separate statement:
  a locking query that waits can otherwise pair the updated catalog with a stale
  summary snapshot. Ordinary appends and clone opening events share one write path.
- Bot controller facts are stored in `bots.controller_status_json`, with an
  observation timestamp that rejects older writes. There is no `bot_status`
  table.

Keeping session display details apart avoids widening the catalog row updated
on every append. Activity does not need its own table. Bot rows do not change
for each internal session event, so their small controller projection stays on
the bot itself.

History forks replay the inherited prefix through the branch point, rather
than copying the source's current summary. Config-only clones project their
fresh opening events. Both paths use the same pure projection as ordinary
appends and the in-memory store. Execution failures are observations, not
replayed event state, and are not inherited by forks.

This is schema revision 23. The migration carries existing sparse activity
rows into `session_summary` and removes `session_activity`; it does not backfill
historical messages or attachments. Greenfield operation is the supported scope
of this implementation. `REQUIRED_SCHEMA_REVISION` and
`LIGHTSPEED_SCHEMA_REVISION` advance together.

### Bots

Activity and pending work are already derived in SQL and stay that way. Add
what PostgreSQL cannot see today:

- **Controller facts on `bots`**, written by an activity only on rare
  transitions: setup status, budget exhausted or reset, last error set or
  cleared, closing and closed. Idle and delivering remain derived from
  `bot_events` and `session_summary`, so an ordinary delivery adds no status
  activity. The additional activity changes workflow history; deployment
  follows the repository's new-namespace policy for breaking workflow changes.
- **`BotListItem.status`**, computed by the roster query with the
  vocabulary the UI draws: `closed`, `paused`, `attention`, `budget`,
  `waiting`, `working`, `idle`. `bots/read` returns it too.
- **Session counts** on `BotListItem` (`openSessionCount`,
  `activeSessionCount`), from `sessions.bot_id` and the origin root index.

Status precedence is closed/closing, paused, waiting for approval, controller
error or degraded setup, budget exhausted with pending work, working, then
idle. An error on the last bot event produces attention only when no new work
is pending or running. Session counts exclude deleted sessions.

With these, `bots/list` and `bots/read` need no Temporal query.
`bots/state/read` keeps its controller query for the detail page.

### Signals published

| Kind | Published after |
| --- | --- |
| `session` | An append on either path, create, rename, metadata, retention, creator stamp, share, a recorded execution failure, soft delete, purge. |
| `bot` | Bot put, close and delete; trigger writes; a bot event insert or outcome; a controller-status write; an activity change of a session in the bot's tree. |
| `environment` | Create, update, connect, disconnect, release, close. Not the periodic liveness stamp. |

## Rejected

- **`NOTIFY` in the append transaction or from a trigger.** It serializes
  commits on the hottest write path.
- **Workers calling gateways directly.** Needs replica discovery and a
  connection per worker and gateway pair.
- **Temporal as the transport.** Too slow for a wake-up, and work must not
  be dispatched onto another role's queue.
- **Logical replication.** Replication slots and privileges are a heavy
  operational requirement for every deployment.
- **A broker such as Redis or NATS.** New infrastructure for every
  self-hosted deployment. The trait leaves room for one later.
- **The workflow computes the summary.** It would grow Temporal history and
  tie display changes to workflow code.
- **Copy user text into the entry's `preview` at admission.** Provider
  adapters treat a user message whose `preview` starts with `[document` as
  a text document (`document_entry` in `crates/llm-runtime/src/blob_io.rs`),
  so a user's own words there could change how the message is sent to the
  model.
- **An asynchronous projector.** A second moving part, and lists would lag
  the log.

## Implementation order

1. Signal contract, in-process implementation, PostgreSQL publisher and
   listener. Publish from the session store methods. Convert the event
   long-poll. Measure with the #121 harness.
2. Convert the gateway waits. Route `EnvironmentSignals` through the signal.
3. `sessions.revision`, the summary projection, migration, in-memory
   parity, API fields. Regenerate the contract and every consumer.
4. Controller-status columns on `bots`, roster fields, and the `bot` and `environment` signals.

## Tests

- **Projection:** vectors for each field, including park and unpark;
  in-memory and PostgreSQL stores produce the same summary; replaying a log
  rebuilds it.
- **Last message:** an assistant message updates the preview from its
  `preview`; a user text message updates it from its inline blob; a user
  message whose blob is in object storage leaves it unchanged; reasoning,
  tool entries and compaction summaries do not count.
- **Stopped execution:** the reaper records a stopped execution with an
  active run; a later run start or end clears it; a promise repair does
  not; `session/read` records one it detects first.
- **Signals:** a table-driven test that every mutating store method
  publishes its kind; a listener reconnect wakes all waiters; with the
  publisher disabled every reader still completes on its fallback timer.
- **Long-poll:** a parked reader returns promptly after an append made by
  another process.
- **Performance, with the #121 harness:** idle readers use the five-second fallback instead of 250 ms polling, and rapid-loop throughput and drain time do not regress. The earlier
  reader fast-path prototype lengthened drains, so compare both.
- **Contract:** the regenerated contract and clients carry the new fields.

## Out of scope

- Pushing changes to clients ([P203](p203-universe-watch-stream.md)).
- Token streaming from model providers.
- A durable change journal or an updated-since list filter.
- Serving `bots/state/read` from PostgreSQL.

Deferred, with the reason:

- **Step-by-step phases** (generating, running tools, compacting). They
  change several times per turn, so they would add a row write and a signal
  to many appends, and in a list they would only flicker.
- **Active run id and pending approval count.** `activity` already says
  that a run is active or that an approval is pending.
- **`botId` on the row.** The UI derives it from the session id.

## Resolved choices and validation

- Promise parks use phase `awaiting` while activity stays `working`; `waiting`
  continues to mean a pending approval.
- The reaper keeps its existing scope of sessions with active or queued runs.
  Reads can discover stopped idle executions.
- Empty, code-only or unavailable message text retains the entire previous
  preview tuple, including role, origin and timestamp.
- Forks project the branch prefix; clones start with their opening configuration.
- Session child counts remain deferred; existing parent-scoped listing suffices.

Implementation is present for the signal bus, store mutation publishers,
reader wakeups, shared summary projection, failure observations, bot roster
status, API fields and generated clients. Local PostgreSQL tests cover summary
replay and in-memory parity, branch behavior, failure races, cross-connection
notifications, inherited access/retention and bot counts. The existing lifecycle,
access and bot-store suites pass with the consolidated schema. Workspace Clippy (all targets, warnings denied), the affected Rust library
suites, contract freshness, consumer type checks/tests/builds, release metadata
and documentation checks pass. Local Temporal tests verify cross-connection
long-poll wakeups, fallback without publishing, execution-failure persistence,
bot delivery and budget parking. A terminated PostgreSQL listener reconnects
and resets its waiters. The full #121 throughput/drain benchmark has not been
run; no capacity or throughput improvement is claimed.

The session-store consistency review added regressions for summary reads after
lock contention, no-op name/metadata updates, unopened-child cleanup and bot
invalidation, and gaps inside fork history. Cleanup preserves dependent children;
head-only reads avoid loading the display projection. Bot invalidation targets
are captured before commit, and publication itself performs no database reads.
