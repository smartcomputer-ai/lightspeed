# Progressive transcript history

Status: implemented and verified.

## Behavior

- Open with the latest 500 events, position at the end, and begin independent
  forward long-polling from the initial response's fenced head.
- Automatically fetch older pages near the top, without a load-more button or
  event-count cutoff. A short initial window may load enough to fill the viewport.
- Preserve the visible entry on prepend. New activity follows the bottom only
  while the reader is there; sending and the existing end button resume following.
- Retry older-page failures without stopping live updates. Abort both directions
  and clear the projection on session changes.
- Recover a transient live-read failure with an immediate non-waiting probe
  from the same cursor. Show a disconnect if that probe also fails, pace further
  retries, and clear it on any successful probe, including an empty response.
  Authorization and event-integrity errors stay immediately visible. Bound live
  requests to their requested wait plus ten seconds of transport/projection time.
- Keep the parent/sub-agent header mounted while run activity refreshes its
  children. Background requests and errors retain the current lineage so the
  transcript viewport does not briefly expand and contract on message submission.

## Input origin

Implemented optional string `origin` on text, text-reference, and media inputs,
accepted input views, and projected context items. It is persisted per entry,
separate from insertion `source` and model role. Platform composer and steering
requests stamp `user:<authenticated-user-id>`; bot event bodies, media, framing,
steering, and context append stamp `event`. Arbitrary application values are
accepted within 1–200 nonblank bytes; old history stays unknown. Audio
preprocessing and context copies retain origin. No schema migration or history
backfill is required.

Verification covers mixed input origins, custom values, projection, context
append, audio transcription, run/steering replay, checkpoints, legacy events,
and authenticated origin stamping despite a conflicting request-body value.

## Message appearance

All user-role inputs use the standard muted message palette, including human,
event, unknown, optimistic, and steering messages. Origin remains available in
the transcript data and demo projections, but adds no visible label or color
emphasis. The composer uses its standard background and muted placeholder with
a visible focus ring.

Every user-role message, regardless of origin, measures its rendered text and
collapses overflow to 160px of text (about 206px including padding and controls).
A mask fades the last 48px; Show more / Show less is keyboard accessible and
exposes its expanded state. A resize observer rechecks line wrapping. Expansion
controls opt into the scroller’s existing message navigation before resizing,
leaving bottom-following while keeping a visible message in place.

Verified origin folding, palettes, optimistic sends, measured overflow,
expansion/collapse, observer cleanup, demo echo provenance, and browser behavior
at desktop/mobile widths in both themes, including expansion at the live end.

## Run statistics

Implemented a horizontal separator with centered, muted statistics: context
at the last model call, cumulative input-plus-output usage, and always duration
(or an explicit unavailable value). The hoverable summary button and chevron
open a popover with input/output totals, model-call count, total tool calls, cache-hit share,
and duration. Token counts
below 1,000 remain exact and larger values use thousands with up to one decimal.
Failures and cancellations keep visible statuses alongside their statistics.

A shared "Show run statistics" checkbox lives in the session-title dropdown
and the active bot-conversation dropdown. It defaults to on and is persisted
in local storage under `lightspeed:user-preferences:<user-id>`, across sessions,
bots, and universes in this browser. Other tabs observe storage changes. Account
switches load that user's preference; blocked storage retains an in-memory choice.
Hiding statistics removes completed-run rows while retaining failure and
cancellation messages. The matching settings buttons remain unchanged.

Statistics remain beneath each run, with no context indicator in the composer.
(Later revised: finished runs now fold behind a run strip that carries the
statistics button; see `platform/README.md` for the current transcript model.)
Context describes measured input to the last call, not a live tokenizer or a
context-capacity gauge. No backend or API changes are needed.

Tool calls are deduplicated by call identity across lifecycle events and context
items, including provider-native tool displays. Failed calls count as calls.
Run totals and call counts remain hidden for partially loaded runs. Individual
usage fields stay unavailable if any generation omitted them; explicit zero
remains distinct from unknown. Missing input on the last generation clears its
context measurement instead of reusing an earlier call's count. Historical
reconstruction and duplicate-event filtering preserve these distinctions.

Verification covers per-call versus cumulative counts, history hydration,
missing and zero counts, failures/cancellation, number
formatting, and popover content. Browser checks exercise keyboard opening,
Escape and focus restoration, stable transcript position, mobile/desktop
geometry and both themes. Preference tests cover shared consumers, reloads,
account isolation, cross-tab changes, invalid data, and blocked storage.

## Implementation

`session/events/read` with `direction: "backward"` uses contiguous event sequence
ranges through the existing fork-aware store boundary. It needs no engine replay, checkpoint,
schema migration, or new persisted projection. Each response includes a
chronological window, an exclusive `nextCursor`, `complete`, and `headCursor`.
For backward reads, pass `nextCursor` as `before` until `complete` is true.
Only the initial head initializes forward consumption. Limits bound events,
not bytes: full message text is retained, as in the existing event API.

The browser stores the loaded event window and folds older pages in chronological
order into a fresh display projection. Live lifecycle facts are retained separately
during that rebuild. Stable tool-group identities survive continuation hydration.
Tool results whose starts are outside the window are displayed explicitly as
continued activity. Run usage is shown as a total only after the start event is
loaded. Current control reconciliation includes the dedicated active run, even
when newer queued runs fill the session summary page.

The scroller's native prepend preservation remains enabled. Its sentinel and
loading indicator sit outside the content element to preserve prepend detection.
Loaded history remains in memory for backward reconstruction; this change does
not add DOM virtualization or page eviction. Initial network/projection work is
bounded independently of session length, while reconstruction cost grows with
the history the reader chooses to load.

## Verification

- Server: 12,005 events, concurrent appends, exact backward coverage, empty
  sessions, invalid cursors/combinations, inherited fork prefixes, and a
  non-replayable log proving reads avoid replay. Existing forward wire requests
  remain compatible and the modified Rust consumers compile.
- Browser projection: tool results crossing boundaries, stable group identity,
  lifecycle isolation, duplicate pages, partial usage, and a run over 10,000 events.
- Hook: recent-first loading, concurrent live/history responses, automatic retry,
  cursor gaps, rejection of an old server's forward prefix, empty sessions,
  session switches, and cancellation.
- Reconnection: dropped fetches and gateway errors recover without flashing a
  warning; repeated failures report an outage, idle probes clear it promptly,
  request deadlines recover stalled sockets, and navigation cancels probes/timers.
- Demo: backward cursor parity and concurrent run creation.
- Lineage: delayed run-status refreshes preserve the existing header and links,
  including alongside a parent link; failed refreshes retain the header, and
  switching sessions or universes never shows the previous scope's children.
- Chromium: desktop and mobile tests with 1,200 runs / 12,001 initial events;
  open at the bottom without fetching older pages, preserve the visible offset
  on prepend and concurrent live arrival, reach the first message, and return
  to the latest message. The mobile anchor stayed within one CSS pixel.
- Rust API/CLI tests, runtime library tests, runtime integration-test compilation,
  TypeScript checks/tests, generated-artifact reproducibility, and production/demo
  builds passed. Credentialed/live runtime suites were not run.
