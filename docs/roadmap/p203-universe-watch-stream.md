# P203 — Universe watch stream

**Status:** Proposed, 2026-11-13. Not started. Depends on
[P202](p202-session-summaries-and-change-signals.md).

Provides the "notification system for metadata updates" asked for in
[#149](https://github.com/smartcomputer-ai/lightspeed/issues/149).

## Goal

A client that shows lists of sessions, bots and environments receives each
changed row as it changes, instead of refetching every list on a timer. The
gateway stays stateless enough to run any number of replicas.

## Current state

- The public API is HTTP JSON-RPC on `POST /rpc`. A request gets one
  response. There is no stream of any kind.
- The contract already defines a notification envelope, `AgentNotification`
  in `crates/api/src/notifications.rs`, but nothing emits it and
  `ServerCapabilities.notifications` is `false`
  (`crates/temporal-runtime/src/gateway/request_context.rs`).
- The web polls: session list 5 s, bot roster 5 s, bot state 3 s, a single
  session 15 s while it runs (`platform/web/src/pages/SessionsPage.tsx`,
  `BotsPage.tsx`).
- The browser never calls the runtime. It calls the Platform backend, which
  checks the member's role and visibility and calls the runtime with a
  service key and the member as actor
  (`platform/backend/src/runtime-client.ts`). For a non-admin it narrows
  `session/list` with `visibleTo`.

After P202 every list row has the data a list shows, and every gateway
process is told which row changed.

## Design

### Streaming results on `/rpc`

A method marked as streaming in the manifest, called with
`Accept: text/event-stream`, answers with a server-sent event stream
instead of one JSON body. This is the pattern of MCP's Streamable HTTP,
which the Configurator MCP already uses.

- Each event's `data` is one JSON-RPC notification from `AgentNotification`.
- The server sends a comment line every 15 s so idle proxies keep the
  connection.
- A stream has a maximum lifetime (proposed: 15 minutes). The server then
  sends the JSON-RPC response for the original request as the last event
  and closes. The client reconnects. This re-authenticates the caller and
  rebalances streams after replicas are added.
- Authentication and authorization are those of any `/rpc` call, checked
  once when the stream starts.
- `ServerCapabilities.notifications` becomes `true`.

The manifest entry, the generated TypeScript and Python clients and the
generated API reference need to learn the streaming flag and the
notifications a method emits.

Server-sent events are enough because the data flows one way. They pass
through HTTP proxies and the Platform's cookie authentication unchanged,
and browsers reconnect them without code. A WebSocket would add an upgrade
path and a second authentication flow for no gain here.

### `universe/watch`

Working name. Parameters:

```text
{ kinds?: ["session", "bot", "environment"], visibleTo?: actorId }
```

`visibleTo` has the meaning it has on `session/list`. A key without the
method group for a requested kind is refused when the stream starts.

Notifications:

```text
session/changed        { session: SessionSummaryView }
session/removed        { sessionId }
bots/changed           { bot: BotListItem }
bots/removed           { botId }
environments/changed   { environment: EnvironmentView }
environments/removed   { environmentId }
watch/reset            {}
```

**A `changed` notification carries exactly the item the matching list
method returns.** The client replaces that row in its cached list by id.
There are no new item types, and no second request per change. The
alternative, notifications with ids only, forces a refetch through the
Platform for every change and a new `ids` filter on each list method.

`watch/reset` means "refetch what you show". The server sends it first on
every stream and whenever it may have missed changes.

There is deliberately no durable cursor for lists. A client that reconnects
refetches its lists, which are sorted by recent change. A stale notification
that arrives after a refetch is corrected by the next one, because every
committed change produces a signal and a fresh read. Session rows also
carry `revision`, so a client can drop a notification older than the row it
holds.

### In the gateway

One hub per universe per gateway process, created with the first subscriber
and dropped with the last:

1. It subscribes to that universe's change signals from P202. For
   sessions it acts only on signals that carry `rev`, which mark a change
   to what a list shows. Signals for appends that only extend the log are
   ignored.
2. It collects changed ids for about 250 ms, per kind, without duplicates.
3. It loads the changed rows in one query per kind. A row that is gone
   becomes a `removed` notification.
4. For each subscriber it filters by requested kinds and visibility and
   queues the notification.

Database cost therefore follows the number of changes, not the number of
subscribers.

**Scale.** The session list is where people watch many sessions at once,
and a universe may have hundreds working in parallel. As an illustration,
take 300 working sessions that each append about once a second and change
visibly (a new message, an activity change) about every ten seconds. The
hub then loads about 30 rows a second, and a subscriber who may see every
session receives about 30 notifications a second. Acting on every append
instead would mean up to 1,200 row loads a second and as many
notifications per subscriber, almost all of them carrying an unchanged row.

Two consequences for clients:

- **Elapsed time is computed locally** from `activitySinceMs`. The server
  does not send a notification because time passed.
- **`updatedAtMs` is not pushed by itself.** A row moves in a list sorted by
  recent change when it changes visibly, and on the slow fallback refetch.

- **Visibility** is one Rust predicate over the loaded row's `access`
  (shared with the universe, or created by the `visibleTo` actor), tested
  for agreement with the `session/list` SQL filter. A session that becomes
  shared arrives as `changed`; a deleted one as `removed`.
- **Slow subscribers.** Each subscriber has a bounded queue that keeps only
  the latest notification per row. If it overflows, the queue is dropped
  and the subscriber gets `watch/reset`.
- **Missed signals.** When the process's listener reconnects, every
  subscriber gets `watch/reset`.
- **Limits.** A cap on concurrent streams per key and per process, counted
  against the gateway's file-descriptor budget. #121 found the default
  limit of 1,024 too low for many open readers and tested 8,192.

### Platform

A new route, `GET /api/v1/universes/:id/watch`, passes one runtime stream
through per browser stream:

- The member gate runs once when the stream starts: viewer role, feature
  switches narrowing `kinds`, and `visibleTo` set to the member for
  non-admins, as for `session/list`.
- The backend keeps no subscription state, so it scales like today.
- Closing either side closes the other.

### Web

- One stream per open universe, shared by all pages through a provider.
- `changed` and `removed` update the cached list and detail queries by id.
  `watch/reset` invalidates them.
- While the stream is up, list refetch intervals rise to a slow fallback
  (proposed: 60 s). While it is down, the current intervals apply.
- The in-browser demo backend serves the same route from its in-memory
  store.

Browsers allow six HTTP/1.1 connections per origin, shared by all tabs. A
watch stream plus a session long-poll per tab can exhaust that with a few
tabs. Serve the Platform over HTTP/2 at the edge, which removes the limit.

### Clients

The TypeScript and Python clients gain a `watch` call that yields
notifications and reconnects. The CLI does not need it.

### Optional: `session/events/follow`

A streaming twin of `session/events/read`: events after a cursor as
`session/event` notifications, with the sequence number as the event id so
a reconnect resumes exactly.

After P202 the existing long-poll already returns as soon as an event lands
and costs nothing while idle. The stream would save one request per batch
and, through the Platform, one visibility check per poll. Build it only if
measurements after P202 show that this matters.

## Rejected

- **WebSocket.** Two-way transport for one-way data; see above.
- **Notifications with ids only.** A refetch per change and a new list
  filter.
- **A durable list cursor.** It needs a universe-wide change order that
  survives concurrent commits. Refetch on reconnect is simpler and correct.
- **The Platform fans out to browsers from one runtime stream per
  universe.** Fewer upstream connections, but the backend becomes stateful
  and must reimplement visibility. Revisit if connection counts require it.

## Implementation order

1. Streaming results on `/rpc`, the manifest flag and the generated clients.
2. The hub and `universe/watch` for sessions.
3. Bots and environments.
4. The Platform route, then the web and the demo backend.

## Tests

- **Gateway:** a change committed by another process arrives as one
  notification; several changes inside the window arrive as one; a deleted
  row arrives as `removed`; a restricted session is not sent to a
  subscriber that may not see it; a listener reconnect and a queue overflow
  each produce `watch/reset`; the stream ends at its maximum lifetime with
  a final response.
- **Visibility:** the predicate and the `session/list` filter agree on a
  table of cases.
- **Platform:** a contributor's stream carries only what they may see; a
  feature that is off removes its kind.
- **Web:** the list updates from a notification without a refetch; a reset
  refetches; polling resumes when the stream is down.
- **Contract:** generated clients expose the streaming method and its
  notifications.

## Open questions

- **Sub-agent sessions.** Does a watch include them by default? A busy
  universe produces many. A `subagents` parameter could exclude them.
- **Unused notifications.** `session/started`, `session/runs/started` and
  `session/runs/completed` were never emitted. Remove them, or keep them
  for `session/events/follow`?
- **One stream per browser.** Is a shared worker across tabs worth it, or
  is HTTP/2 at the edge enough?
