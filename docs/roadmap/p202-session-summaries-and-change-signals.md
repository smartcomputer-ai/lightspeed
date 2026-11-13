# P202 — Session and bot summaries with a change signal

**Status:** Proposed, 2026-11-13. Not started. Decided: PostgreSQL
`LISTEN`/`NOTIFY` is the first transport for the change signal, and the
session summary holds the fields listed under
[Session summary](#session-summary), including attachments. Step-by-step
phases of a run are deferred.

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
   gateway stops polling PostgreSQL and Temporal to find out.

The two parts change the same store functions, and the row revision added for
the first is what readers compare after the second wakes them. They are one
piece of work. The signal lands first because it needs no migration and no
contract change.

## Current state

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

PostgreSQL `LISTEN`/`NOTIFY`, behind a small trait with a second,
in-process implementation for single-process runs and tests. Every runtime
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
- **Volume.** At most one notification per 20 ms per publishing process,
  however many rows changed in that window.
- **Versioning.** A receiver that cannot parse a payload, or sees an
  unknown `v`, wakes every waiter. Mixed versions during a rolling upgrade
  therefore stay correct.
- **Not stored.** PostgreSQL delivers a notification only to listeners
  connected at that moment, on the primary. Nothing is written to a table
  and nothing is replayed.

A process that both publishes and subscribes also delivers the signal
locally at once, without waiting for the batch.

### Subscribing

Each process with the `gateway` or `environment-gateway` role holds one
listening connection (`sqlx::postgres::PgListener`) and an in-process
registry of waiters keyed by `(universe, kind, id)` and by universe. When
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
one per gateway process, independent of the number of users. The listener
must be built through the same connection path as the pool so that Azure
workload-identity authentication applies; verify this during implementation.

### What changes for readers

- **Event long-poll.** `read_session_events` waits for a `session` signal
  for its session instead of sleeping 250 ms. Fallback timer: 5 s.
- **Gateway waits.** Each wait in `gateway/service/` wakes on the session's
  signal. They keep the 500 ms timer as the fallback, because an admission
  that is refused is reported through workflow status without an append and
  therefore produces no signal.
- **Environment signals.** `nudge_work` and `revoke` also publish an
  `environment` signal, so an API-only gateway process reaches the process
  that runs the reconciler and holds the routes.

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
| `activitySinceMs` | already stored as `session_activity.since_ms` | When the current activity or phase began. |
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
most once per turn, not per tool call. A client can tell the final answer
from an interim message without another field: the session is idle,
`lastRun` completed, and `lastMessageRole` is assistant.

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
- The write bumps `revision` and publishes a `session` signal.

It is store metadata like the display name, not an event. An idle session
whose execution has stopped is still reported only by `session/read`,
because the reaper does not check sessions without a run.

### Storage

- `sessions.revision bigint`: bumped by the append only when the projection
  changed something a list shows, and by every other write to the row or to
  the two tables below.
- `session_activity` gains a nullable `phase`, and `since_ms` is reset when
  the phase changes. It keeps its current lifetime: a row only while the
  session is not idle, which the bot roster query relies on.
- New `session_summary`, keyed by `(universe_id, session_id)`, holding the
  last-run, last-message, active-environment, attachment and
  execution-failure fields. It is written only when one of them changes, as
  `write_activity` does today, so an ordinary append does not rewrite it.

The fork and clone path (`append_events_in_tx`) runs the same projection.
Today it skips the activity write.

This is schema revision 23: raise `REQUIRED_SCHEMA_REVISION` and
`LIGHTSPEED_SCHEMA_REVISION` together and run
`scripts/release/verify-metadata.sh`. There is no backfill; an existing
session gains its summary with its next event.

### Bots

Activity and pending work are already derived in SQL and stay that way. Add
what PostgreSQL cannot see today:

- **`bot_status` row**, written by a controller activity only on rare
  transitions: setup status, budget exhausted or reset, last error set or
  cleared, closing and closed. Idle and delivering remain derived from
  `bot_events` and `session_activity`, so an ordinary delivery adds no
  activity call. The controller change must be replay-safe for running
  controller workflows.
- **`BotListItem.status`**, computed by the roster query with the
  vocabulary the UI draws: `closed`, `paused`, `attention`, `budget`,
  `waiting`, `working`, `idle`. `bots/read` returns it too.
- **Session counts** on `BotListItem` (`openSessionCount`,
  `activeSessionCount`), from `sessions.bot_id` and the origin root index.

With these, `bots/list` and `bots/read` need no Temporal query.
`bots/state/read` keeps its controller query for the detail page.

### Signals published

| Kind | Published after |
| --- | --- |
| `session` | An append on either path, create, rename, metadata, retention, creator stamp, share, a recorded execution failure, soft delete, purge. |
| `bot` | Bot put, close and delete; trigger writes; a bot event insert or outcome; a `bot_status` write; an activity change of a session in the bot's tree. |
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
4. `bot_status`, roster fields, and the `bot` and `environment` signals.

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
- **Performance, with the #121 harness:** idle readers issue close to no
  SQL, and rapid-loop throughput and drain time do not regress. The earlier
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

## Open questions

- **Phase name.** Confirm `awaiting` with the UI, and that a run parked on
  promises stays `working` in the top-level `activity`, where waiting means
  "needs a person".
- **Idle sessions with a stopped execution.** Should the reaper also check
  open sessions without a run, so the list and the read always agree?
- **Long assistant messages that open with code.** The 256-character
  `preview` can be empty after reduction when a message starts with a long
  code block. Is leaving the previous preview acceptable there?
- **Forks and clones.** Does a fork start with an empty summary, or copy
  its source's summary at the branch point?
- **Child counts.** Does a session row need a sub-agent count, or does the
  UI keep deriving it from `session/list` with `parent`?
