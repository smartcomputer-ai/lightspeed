# U100: Missing Server Implementations

Features the web UI already draws, and the in-browser demo backend
(`platform/web/src/demo`) already serves, but the core server does not
provide yet. Against a real deployment the UI degrades gracefully until the
server side lands. Each section describes what the UI expects, why it needs
the server, and the proposed server change.

## Session list: latest message preview

**Status:** Web UI and demo backend done, 2026-11-11. Server not started.

### What the UI shows

Each row of the Sessions list shows the session's latest message as a muted
second line under the title, truncated to the row width. Rows without it
show the title alone, so the list keeps working while the server does not
send it.

The web reads an optional field on each session summary of
`GET /api/v1/universes/{id}/sessions` (`session/list`):

| Field | Type | Meaning |
| --- | --- | --- |
| `lastMessagePreview` | `string`, optional | The latest message of the session, user or agent, as one line of plain text of at most 200 characters, ending in `…` when cut. Omitted when the session has no message yet. |

The field is declared on `SessionSummary` in `platform/web/src/api.ts`,
marked as demo-only for now.

### Why it needs the server

`SessionSummaryView` (`crates/api/src/sessions.rs`) carries no message text,
and nothing in the store records a recent message per session. Deriving it
in the browser would mean reading each listed session's events, one request
per row on every list load and refresh. The preview belongs in the list
response, computed when messages are written.

### Reference behaviour

The demo computes the preview in `lastMessagePreviewOf`
(`platform/web/src/demo/store.ts`), and `platform/web/src/demo/router.test.ts`
pins it ("lists each session with its latest message as one plain, bounded
line"). The server should match it:

- The latest context message by role user or assistant, including steering
  input. Reasoning, tool calls, system entries and markers do not count.
- Markdown reduced to its words: fenced code blocks dropped, links and images
  reduced to their text, emphasis markers, headings, quotes and list markers
  removed. Underscores stay, so `snake_case` names survive.
- Whitespace, including line breaks, collapsed to single spaces and trimmed.
- At most 200 characters. A longer text is cut to 199 characters, trimmed at
  the end, and followed by `…`.
- A message that is empty after reduction (a code block alone, or an
  attachment without text) is skipped in favour of the message before it.

### Proposed implementation

1. **Store projection.** Keep the preview beside the session's other list
   projections, following `session_activity`. Either add a nullable column to
   `sessions`, or add a `session_last_message` table keyed by
   `(universe_id, session_id)` holding the preview and the `observed_at_ms`
   of its message. Write it in the same transaction that appends events
   (`crates/store-pg/src/session.rs`, where the committed entries are
   projected and `write_activity` runs) whenever the committed entries
   include a user or assistant message. Read it in the list query the way
   `SESSION_ACTIVITY` is read, and in the single-session summary.
2. **Message text source.** Context messages may carry inline text or a
   `textRef` to a content blob. Inline text can be reduced directly. For a
   `textRef`, decide between reading a bounded prefix of the blob inside the
   append path and recording no preview for that message, which leaves the
   previous one. The append path should not grow unbounded blob reads.
3. **Migration.** Add the next migration in `crates/store-pg/migrations/`,
   raise `REQUIRED_SCHEMA_REVISION` in `crates/store-pg/src/migrations.rs`
   and `LIGHTSPEED_SCHEMA_REVISION` in `release/metadata.env` together, and
   run `scripts/release/verify-metadata.sh`. Existing sessions can start
   without a preview and gain one with their next message; a backfill from
   stored events is optional.
4. **API.** Add `last_message_preview: Option<String>` to `SessionSummaryView`
   with wire documentation and `skip_serializing_if = "Option::is_none"`.
   Fill it where summaries are built: `session_summary_view` in
   `crates/temporal-runtime/src/gateway/service/mod.rs`, the in-memory session
   storage in `crates/harness/src/storage/session.rs`, and the CLI and test
   constructors that build the struct. Bot state lists descendants as
   `SessionSummaryView` too (`crates/api/src/bots.rs`), so they gain the field
   for free.
5. **Contract and clients.** Regenerate the API contract and every TypeScript
   consumer, following "Regenerate the public API and consumers" in
   `docs/documentation/development/changing-contracts.md`.
6. **Web.** Once the generated `SessionSummaryView` carries the field, remove
   the demo-only note from `SessionSummary` in `platform/web/src/api.ts`. The
   Sessions row needs no change.

### Access

The preview is message content, so it must reach exactly the audience that
can read the transcript. `session/list` only returns sessions the caller can
read, so carrying it there adds no new audience. Keep it that way if list
visibility ever widens beyond read access.

### Tests

- Store: the projection updates when a user or assistant message is
  appended, ignores other events, survives restarts, and is bounded and
  normalised as above.
- List and read: the field appears on summaries, is omitted for sessions
  without messages, and respects access filtering.
- Contract: the regenerated contract and clients include the field.
