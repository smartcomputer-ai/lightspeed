# P203 — Universe watch stream

**Status:** Implemented and validated, 2026-11-14. Depends on
[P202](p202-session-summaries-and-change-signals.md).

Provides the "notification system for metadata updates" asked for in
[#149](https://github.com/smartcomputer-ai/lightspeed/issues/149).

## Goal

A client that shows lists of sessions, bots, environments and workspaces receives each
changed row as it changes, instead of refetching every list on a timer. The
gateway stays stateless enough to run any number of replicas.

## Scope and decisions

One watch per active universe in each browser tab, shared across pages. It
sends every authorized session regardless of list filters, including children,
closed sessions and controlled sessions. The browser applies the current view's
filters to incoming rows. Ordinary list requests retain server-side filtering
and pagination.

The watch covers session summaries, bot rosters, environments and VFS
workspaces. Detailed bot controller state keeps its existing polling. Model
token streaming, `session/events/follow` and cross-tab connection sharing are
deferred. Existing legacy notification types remain for compatibility.

## Design

### Streaming results on `/rpc`

A method marked as streaming in the manifest, called with
`Accept: text/event-stream`, answers with a server-sent event stream
instead of one JSON body. This is the pattern of MCP's Streamable HTTP,
which the Configurator MCP already uses.

- Each event's `data` is one JSON-RPC notification from `AgentNotification`.
- The server sends a comment line every 15 s so idle proxies keep the
  connection.
- A stream has a maximum lifetime (15 minutes). The server then
  sends the JSON-RPC response for the original request as the last event
  and closes. The client reconnects. This re-authenticates the caller and
  rebalances streams after replicas are added.
- Authentication and authorization are those of any `/rpc` call, checked
  once when the stream starts.
- `ServerCapabilities.notifications` is `true`.

The manifest, generated TypeScript and Python clients, and generated API
reference expose the streaming flag and the notifications a method emits.

Server-sent events are enough because the data flows one way. They pass
through HTTP proxies and the Platform's cookie authentication unchanged,
and clients reconnect them after completion or transient failures. The browser
uses streaming `fetch` so the same transport also works with the demo backend.
A WebSocket would add an upgrade path and a second authentication flow for no gain here.

### `universe/watch`

Parameters:

```text
{ kinds?: ["session", "bot", "environment", "workspace"], visibleTo?: actorId }
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
workspaces/changed     { workspace: VfsWorkspaceView }
workspaces/removed     { workspaceId }
watch/reset            {}
```

**A `changed` notification carries exactly the item the matching list
method returns.** The client replaces that row in its cached list by id.
There are no new list item types or extra list requests per change. An open
workspace tree still fetches the manifest for its new head. The
alternative, notifications with ids only, forces a refetch through the
Platform for every change and a new `ids` filter on each list method.

`watch/reset` means "refetch what you show". The server sends it first on
every stream and whenever it may have missed changes.

There is deliberately no durable cursor for lists. A reconnect refetches the
visible queries. Session and workspace revisions reject older rows; the web
replays changes that overlap an HTTP read after its response is cached.
Periodic reads repair missed signals and ordering changes that are not pushed.

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
instead would mean about 300 row loads a second and as many
notifications per subscriber, almost all of them carrying an unchanged row.

Two consequences for clients:

- **Elapsed time is computed locally** from `activitySinceMs`. The server
  does not send a notification because time passed.
- **`updatedAtMs` is not pushed by itself.** A row moves in a list sorted by
  recent change when it changes visibly, and on the slow fallback refetch.

- **Visibility** is one Rust predicate over the loaded row's `access`
  (shared with the universe, or created by the `visibleTo` actor), tested
  for agreement with the `session/list` SQL filter. A session that becomes
  shared arrives as `changed`. A missing session sends `removed` to an
  unrestricted stream and `watch/reset` to a restricted stream: its access
  record is gone, so disclosing its ID would leak private session identities.
- **Slow subscribers.** Each subscriber has a bounded queue that keeps only
  the latest notification per row. The queue is limited to 256 rows or 2 MiB.
  If it overflows, the queue is dropped and the subscriber gets `watch/reset`.
- **Missed signals.** When the process's listener reconnects, every
  subscriber gets `watch/reset`.
- **Limits.** At most 64 streams per API key and 1,024 per gateway process.
  Platform streams share its service key, so that limit also caps simultaneous
  browser streams using that key on one runtime replica. Review it against the
  deployment's file-descriptor budget before increasing concurrency. A hub
  resets rather than retaining more than 2,048 pending IDs.

### Platform

A new route, `GET /api/v1/universes/:id/watch`, passes one runtime stream
through per browser stream:

- The member gate runs once when the stream starts: viewer role, feature
  switches narrowing `kinds`, and `visibleTo` set to the member for
  non-admins, as for `session/list`.
- The backend keeps no subscription state, so it scales like today.
- Closing either side closes the other.
- Platform rotates each browser connection after 60 seconds, rerunning cookie,
  membership and feature admission on reconnect. Revocation therefore takes
  effect within that connection lifetime, rather than the runtime's 15 minutes.

### Web

- One stream per open universe, shared by all pages through a provider.
- `changed` and `removed` update the cached list and detail queries by id.
  `watch/reset` invalidates them.
- While the stream is up, list refetch intervals rise to a slow fallback
  (60 s). While it is down, the current intervals apply.
- Workspace changes patch the picker immediately. A new head invalidates the
  open tree; the file view reads by the tree's immutable blob reference. An
  unsaved draft retains its original revision and blocks saving after an
  external workspace change. A successful local save or revision-guarded move
  can advance that draft's base; there is no automatic merge. Workspace reads
  poll every 10 seconds while disconnected.
- Changes overlapping an HTTP read are replayed after its response is cached.
  Loaded page tails and their continuation cursors are preserved.
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
with a five-second fallback read when signals are lost. The stream would save
one request per batch and, through the Platform, one visibility check per poll. Build it only if
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

## Implementation and validation

- Streaming `/rpc` metadata, generated contracts, TypeScript and Python watch
  iterators, bounded hub queues and access filtering are implemented.
- The Platform proxy, browser provider and demo stream cover all four kinds.
- Workspace store mutations signal only after commit, including changes from
  agent tools, API writes, renames and deletion. Failed revision checks emit
  nothing. No new tables or migrations were needed.
- Regression coverage includes stream framing/cancellation, queue overflow,
  visibility and groups, cache overlap/filter/pagination behavior, workspace
  draft preservation, and HTTP delivery through independent PostgreSQL buses.
- Validation passed: API and store tests, all 649 runtime unit tests, the live
  HTTP/PostgreSQL watch test, and workspace Clippy with all targets and warnings
  denied. The live test covers all four kinds, list-row parity, inherited session
  visibility, deletion, reset and failed workspace compare-and-set writes.
- TypeScript typechecks, SDK, Platform backend/web and Configurator tests,
  Python mypy/pytest, generated-artifact reproducibility, development/identity
  checks, production/demo builds and documentation checks passed. Earlier
  failing fixtures were corrected and their affected suites rerun.
- A headless Chrome smoke test confirmed live saved-content refresh, draft
  conflict protection, discard/reload and draft preservation after deletion,
  with no browser errors.
- Production throughput at thousands of simultaneous sessions is not
  benchmarked here. Credentialed model suites and the deferred features above
  are outside this validation pass.
