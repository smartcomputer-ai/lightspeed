# P44: Agent Session Store and Event Log

**Status**
- Complete

## Goal

Make the session store the durable event-sourced persistence boundary for the
first Forge agent runtime.

This phase intentionally keeps the model small:

- one append-only event log per session
- no separate `JournalId`
- no separate `JournalStore` public interface
- no fork journals
- no journal rewrite or migration machinery
- no hash chain or content-addressed event entries yet
- no transcript/projection/subagent/history-control replacement model

The key distinction is not "session versus journal". The useful distinction is:

```text
SessionRecord
  durable session metadata and append head

SessionEntry
  persisted event-log entry for that session

SessionState
  bounded state produced by replaying session entries
```

The session is the durable stream identity. The state is the replay result.

## Design Position

The current implementation mixes event payloads with persistence envelope data:

- `AgentEvent` carries `event_id`
- `AgentEvent` carries `journal_seq`
- `AgentEvent` carries `session_id`
- many child ids also bake in `SessionId`

P44 should split this into a smaller shape:

```text
SessionStore
  create/load/list sessions
  append/read session entries

SessionEntry
  session position, timestamp, joins, event payload

AgentEvent
  reducer payload only

SessionState
  replayed control state for one session
```

There is no separate session binding in this first cut because there is no
separate journal object to bind to.

## Core Invariants

- A session has a stable `SessionId`.
- A session has one append-only event log.
- Session entries are ordered by a monotonically increasing `EventSeq` within
  that session.
- The session store assigns event sequence numbers.
- Event payloads do not carry their own sequence.
- Event payloads do not carry `SessionId`.
- The first version has no durable session-state snapshots.
- Replaying session entries from the beginning must reconstruct the same
  bounded `SessionState`, modulo explicitly out-of-band runner state.

## Target Model Types

### Agent handle

Most agents are configured in code. We still need a persisted lookup key for
querying the sessions that belong to a configured agent.

```rust
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentHandle(String);
```

`AgentHandle` is not an agent definition catalog. It is a stable grouping key
such as `forge.default`, `factory.reviewer`, or a host-defined id.

### Session position

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventSeq(pub u64);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPosition {
    pub seq: EventSeq,
}
```

`EventSeq` and `SessionPosition` are local to a session. If a globally unique
event reference is needed, compose `SessionId + SessionPosition` at the
boundary rather than storing the session id in every position.

### Session record

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id: SessionId,
    pub agent_handle: AgentHandle,
    pub head: Option<SessionPosition>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}
```

`SessionRecord` is storage metadata:

- which agent grouping owns this session?
- what is the current append head?
- when was this session created or last appended to?

It should not contain a durable reducer cursor. Replay progress is handled by
the active runner while applying entries from the session log.

### Session entry envelope

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEntry {
    pub position: SessionPosition,
    pub observed_at_ms: u64,
    pub joins: AgentEventJoins,
    pub event: AgentEvent,
}
```

The entry envelope owns persistence coordinates and event metadata. The reducer
consumes ordered `SessionEntry` values, not unsequenced events.

### Uncommitted session event

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UncommittedSessionEvent {
    pub observed_at_ms: u64,
    pub joins: AgentEventJoins,
    pub event: AgentEvent,
}
```

The store commits uncommitted events into sequenced `SessionEntry` values.

### Agent event payload

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEvent {
    pub kind: AgentEventKind,
}
```

`AgentEvent` should become the reducer payload, not the persistence envelope.

Remove from `AgentEvent`:

- `event_id`
- `journal_seq`
- `session_id`
- `observed_at_ms`
- `joins`

Move `observed_at_ms` and `joins` to `SessionEntry` and
`UncommittedSessionEvent`.

Remove from `AgentEventJoins`:

- `parent_event_id`

If event-to-event causality becomes necessary later, add an explicit typed
`SessionPosition` reference. Do not use free-form event ids.

### Event joins

The target joins are reducer/query helpers:

```rust
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEventJoins {
    pub run_id: Option<RunId>,
    pub turn_id: Option<TurnId>,
    pub effect_id: Option<EffectId>,
    pub tool_batch_id: Option<ToolBatchId>,
    pub tool_call_id: Option<ToolCallId>,
    pub submission_id: Option<SubmissionId>,
    pub correlation_id: Option<CorrelationId>,
    pub parent_effect_id: Option<EffectId>,
}
```

They should not carry session ownership. The surrounding `SessionEntry`
already identifies the session.

## Session Store Interface

The exact implementation backend is out of scope. The desired interface is:

```rust
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn create_session(
        &self,
        request: CreateSession,
    ) -> Result<SessionRecord, SessionStoreError>;

    async fn load_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionRecord>, SessionStoreError>;

    async fn list_agent_sessions(
        &self,
        request: ListAgentSessions,
    ) -> Result<Vec<SessionRecord>, SessionStoreError>;

    async fn append(
        &self,
        request: AppendSessionEvents,
    ) -> Result<AppendSessionEventsResult, SessionStoreError>;

    async fn read_after(
        &self,
        request: ReadSessionEvents,
    ) -> Result<SessionPage, SessionStoreError>;

    async fn head(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionPosition>, SessionStoreError>;
}
```

Supporting records:

```rust
pub struct CreateSession {
    pub session_id: SessionId,
    pub agent_handle: AgentHandle,
    pub created_at_ms: u64,
}

pub struct ListAgentSessions {
    pub agent_handle: AgentHandle,
    pub limit: usize,
}

pub struct AppendSessionEvents {
    pub session_id: SessionId,
    pub expected_head: Option<SessionPosition>,
    pub events: Vec<UncommittedSessionEvent>,
}

pub struct AppendSessionEventsResult {
    pub entries: Vec<SessionEntry>,
    pub head: Option<SessionPosition>,
}

pub struct ReadSessionEvents {
    pub session_id: SessionId,
    pub after: Option<EventSeq>,
    pub limit: usize,
}

pub struct SessionPage {
    pub entries: Vec<SessionEntry>,
    pub next_after: Option<EventSeq>,
    pub complete: bool,
}
```

`SessionStore` must not return `ModelError`. `ModelError` is reserved for
deterministic model, lifecycle, reducer, and decider invariant failures. The
session store is an effectful persistence boundary, so it needs a storage
error type that can represent missing sessions, compare-and-set conflicts,
bad paging requests, and backend failures without pretending those are reducer
errors.

First-cut error shape:

```rust
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionStoreError {
    #[error("session already exists: {session_id}")]
    SessionAlreadyExists { session_id: SessionId },

    #[error("session not found: {session_id}")]
    SessionNotFound { session_id: SessionId },

    #[error("expected head mismatch for {session_id}: expected {expected:?}, actual {actual:?}")]
    ExpectedHeadMismatch {
        session_id: SessionId,
        expected: Option<SessionPosition>,
        actual: Option<SessionPosition>,
    },

    #[error("invalid page limit: {limit}")]
    InvalidLimit { limit: usize },

    #[error("session store failure: {message}")]
    Store { message: String },
}
```

Backend implementations may wrap their native errors into `Store`. If a store
calls deterministic model helpers internally, it should translate those
failures deliberately rather than exposing `ModelError` through the public
store trait.

Append rules:

- `create_session` creates an empty session log with no head.
- `append` rejects unknown sessions.
- `append` rejects `expected_head` from another session.
- `append` rejects the write if `expected_head` does not match the current
  head.
- `append` assigns contiguous sequence numbers starting at `1`.
- `append` preserves caller-provided `observed_at_ms`.
- `append` returns the committed `SessionEntry` values.

Read rules:

- `read_after { after: None }` starts at sequence `1`.
- `read_after { after: Some(n) }` returns entries with `seq > n`.
- `limit == 0` should return an empty complete page or be rejected explicitly;
  pick one behavior and test it.
- The session store does not reduce state and does not execute effects.

## Why There Is No `update_reduced_to`

Do not put durable reducer progress in `SessionRecord` for this phase.

In the first event-sourced model, the durable facts are:

- the session record
- the session event log

A separate `update_reduced_to` cursor adds another mutable checkpoint that can
drift from event replay. It is useful for projection workers, snapshots, or
multi-consumer read models, but that is not the first-cut agent core.

For now:

- `SessionRecord.head` is the append head.
- The active runner may keep an in-memory `reduced_to` cursor while stepping.
- On resume, replay session entries from the beginning.

If we later add session-state snapshots or persistent background projections,
they should have their own checkpoint table, not reuse the session record as a
generic cursor.

## Session Creation Flow

The first-cut flow is:

```text
1. allocate SessionId
2. SessionStore::create_session(session_id, agent_handle)
3. SessionStore::append(SessionOpened { config })
4. reduce appended entry into SessionState
```

`SessionOpened` initializes reducer state. `SessionRecord` is the lookup and
append-head metadata.

## Append and Replay Flow

For a normal external input:

```text
1. load SessionRecord by SessionId
2. build UncommittedSessionEvent
3. SessionStore::append with expected current session head
4. reduce committed SessionEntry values into SessionState
5. decide next effects from updated SessionState
```

For resume:

```text
1. load SessionRecord
2. initialize empty SessionState for that session
3. SessionStore::read_after(None)
4. reduce all entries in order
5. continue deciding effects from updated SessionState
```

Replay from the beginning is the only first-cut resume path. Session-state
snapshots can be added later as a replay optimization if measured replay cost
justifies the extra persistence contract.

`SessionState` may also carry the reducer cursor for diagnostics and runner
handoff:

```rust
pub struct SessionState {
    pub session_id: SessionId,
    pub reduced_to: Option<SessionPosition>,
    // derived control state
}
```

The session id belongs in `SessionState` because this is the live state for a
named session. It does not need to be repeated inside every event payload.

## Event Markers

Store structure and reducer-visible events have different jobs.

Persisted as session metadata:

- session existence
- agent handle
- append head
- created/updated timestamps

Persisted as session entries:

- `SessionOpened`
- `SessionConfigUpdated`
- `RunRequested`
- `FollowUpInputAppended`
- `SessionClosed`
- turn lifecycle events
- effect intents, receipts, and stream frames
- observations
- context pressure and compaction events
- tool registry/profile/override changes
- confirmation responses

Do not add `SessionCreated` as an authoritative reducer event in this phase.
`SessionOpened` is the reducer-visible initialization boundary; session
existence is storage metadata.

## Removing SessionId From Domain Records

The rule is:

```text
Keep SessionId where the record identifies, loads, or stores a session.
Remove SessionId where the record is already inside a session replay context.
```

Keep `SessionId` on:

- `SessionRecord`
- `SessionState`
- external APIs that load/control a session

Remove `SessionId` from:

- `AgentEvent`
- `AgentEffectIntent`
- `AgentEffectReceipt`
- `EffectStreamFrame`
- `RunId`
- `EffectId`
- `ToolBatchId`
- `TurnId`

The desired id direction is local replay ids:

```rust
pub struct RunId(pub u64);
pub struct TurnId(pub u64);
pub struct ToolBatchId(pub u64);
pub struct EffectId(pub u64);
```

Parent/child relationships belong in event joins and run state, not inside the
child id itself. `SessionState` keeps last-observed id cursors derived during
replay and computes the next ids from those cursors; it does not persist or
restore a separate allocator object.

If a globally unique external reference is needed, compose it at the boundary:

```text
SessionId + RunId
SessionId + EventSeq
SessionId + EffectId
```

Do not bake the session id into every internal child id just to get global
uniqueness.

## Desired Code Changes

### Model

- Add `AgentHandle`, `EventSeq`, and `SessionPosition`.
- Rename `JournalSeq` to `EventSeq` or equivalent.
- Replace `AgentEvent` as persistence envelope with `SessionEntry`.
- Add `UncommittedSessionEvent`.
- Remove `event_id` from `AgentEvent`.
- Remove `journal_seq` from `AgentEvent`.
- Remove `session_id` from `AgentEvent`.
- Move event `observed_at_ms` to `SessionEntry`/`UncommittedSessionEvent`.
- Move event `joins` to `SessionEntry`/`UncommittedSessionEvent`.
- Remove `parent_event_id` from `AgentEventJoins`.
- Rename `SessionState.latest_journal_seq` to
  `SessionState.reduced_to: Option<SessionPosition>`.
- Remove embedded `SessionId` fields from session-local child ids and effect
  records where callers already operate inside a session context.
- Use flat session-local `RunId`, `TurnId`, `ToolBatchId`, and `EffectId`
  counters.
- Replace durable allocator state with replay-derived `last_*_id` cursors on
  `SessionState`.

### Storage

- Replace the current session-scoped `JournalStore` with `SessionStore`.
- Add `SessionRecord` and session append/read records.
- Add `SessionStoreError`; do not expose `ModelError` from storage traits.
- Remove `update_reduced_to` from the store contract.
- Keep in-memory store implementations for tests.

### Reducer and Stepper

- Make reducer APIs consume `SessionEntry` rather than `AgentEvent`.
- Validate ordered `SessionEntry.position.seq` values during replay.
- Use `SessionEntry.observed_at_ms` anywhere reducer logic currently reads
  `event.observed_at_ms`.
- Use `SessionEntry.joins` anywhere reducer logic currently reads
  `event.joins`.
- Update the local stepper to:
  - create a session record
  - append uncommitted session events
  - reduce committed entries
  - resume by replaying the session log from the beginning

### Tests

- Session store tests should assert session-local sequence assignment.
- Session store tests should not assert duplicate `event_id` behavior.
- Session store tests should assert expected-head compare-and-set behavior.
- Session store tests should assert typed `SessionStoreError` variants for
  missing sessions, duplicate sessions, and expected-head conflicts.
- Reducer tests should fail if entries are missing positions or are applied out
  of order.
- Stepper tests should resume by replaying the session log from the beginning.

## Out Of Scope

- Separate journal identity
- Fork journals
- Journal lineage metadata
- Journal rewrite/migration
- Entry hash chains
- Cross-session journal sharing
- Production SQLite/Postgres/CXDB schemas
- CLI session listing/query UX
- Temporal workflow implementation

These can be added later by splitting the session event log into a journal
object if we have a concrete need for shared or forked histories.

## Done When

- The model has `AgentHandle`, `EventSeq`, and `SessionPosition`.
- `AgentEvent` is no longer a persistence envelope.
- Session entries, not event payloads, carry sequence and observed timestamp.
- Session persistence is represented by a single `SessionStore` contract.
- `SessionStore` returns `SessionStoreError`, while reducer/model APIs continue
  to use `ModelError`.
- `SessionState` tracks `reduced_to: SessionPosition`.
- The in-memory stepper can create, append, replay, and resume a single linear
  session event log without durable session-state snapshots.
- `cargo test -p forge-agent` passes.

## Completed

- Added `storage::session` with `SessionStore`, `InMemorySessionStore`,
  `SessionStoreError`, session records, append/read requests, `SessionEntry`,
  and `UncommittedSessionEvent`.
- Removed the old `JournalStore` and in-memory journal helper.
- Reduced `AgentEvent` to reducer payload shape; session position, timestamp,
  and joins now live on the session-entry envelope.
- Removed embedded session ids from child ids and effect intent/receipt/frame
  records; session-scoped references are composed at the boundary.
- Flattened run, turn, tool-batch, and effect ids to session-local counters and
  replaced allocator state with replay-derived last-id cursors.
- Moved `SessionState` from `latest_journal_seq` to `reduced_to`.
- Updated the local stepper to create a session, append uncommitted events to
  the session store, and reduce committed entries.
- Removed durable session-state snapshot persistence from the first-cut core;
  resume replays the session log from the beginning.
- Verified with `cargo test -p forge-agent`.
