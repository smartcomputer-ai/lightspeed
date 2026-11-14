# P204 — Authors and times on transcript messages

**Status:** SDK facilities implemented, 2026-11-14. UI integration and rendering
are deferred to the product designer. No runtime or Platform API changes needed.

Addresses
[#157](https://github.com/smartcomputer-ai/lightspeed/issues/157). The current
scope supplies TypeScript SDK helpers and integration documentation; visual
changes remain separate.

## Goal

In a session that several people use, each user message shows who sent it
and when, so the transcript can show other people's names and leave out the
viewer's own.

## Current state

The runtime already records both facts, and the Platform already has the
names:

- **Time.** Every session event carries `observedAtMs`.
- **Author on the message.** A user message item carries `origin`. For
  input sent through the Platform it is `user:<platform user id>`: the
  backend derives it from the signed-in user and ignores a value in the
  request body (`platform/backend/src/routes/gateway.ts`, run start and
  steer). Bot deliveries carry `event`.
- **Author on the admitting event.** `runAccepted` and `runSteeringAccepted`
  carry `requestedBy`, the authenticated attribution
  (`{ "kind": "actor", "id": "<platform user id>" }`).
- **Names.** `GET /api/v1/universes/:id/members` returns each member's user
  id, name and role to every member (emails only to admins).

The web drops them. In `platform/web/src/lib/sessions/transcript.ts`:

- `contextEntriesApplied` calls `applyItems` without the event's time, so a
  message entry has no timestamp.
- `origin` is copied to the entry and used only to recognise a bot delivery.
- `runAccepted` ignores `requestedBy`.

## Design

### Time

The SDK derives `sentAtMs` from existing event timestamps; it is not a new
stored timestamp or a read receipt:

- For run input, the `observedAtMs` of the run's `runAccepted` event. A
  queued message enters context later than it was sent, and the acceptance
  time is when the server accepted the input, not a device timestamp.
- For steering input, the `observedAtMs` of `runSteeringAccepted`.
- When the admitting event is not in the loaded window, and for assistant
  messages, the `observedAtMs` of the event that carried the item.

History loads newest first, so an item can be loaded before its admitting
event. The entry then takes the item event's time and is corrected when the
older page arrives.

### Author

The entry's author is the user id in `origin` when it has the form
`user:<id>`. It is on the item itself, so it is available whichever pages
are loaded. `requestedBy` on the admitting event is the authenticated
record of the same fact; when both are loaded and disagree, show the
`requestedBy` actor.

- `origin` `event` stays a bot delivery, rendered as today.
- Any other origin, or none, supplies no author by itself. An authenticated
  actor is still available when recorded on the matching acceptance event.
  Sessions driven by other API clients set their own origin strings.

### Names

- Resolve user ids through the members list, loaded once per universe and
  cached.
- An unmatched actor may be a former member or an external application actor.
  Do not label it a former member without evidence; the designer owns the fallback.
- Show names only where they add information: when the session has more
  than one distinct author, and not on the viewer's own messages.

### Demo backend

When UI work resumes, the fixtures can gain a second author and message times,
so the multi-author rendering can be seen without a deployment.

## When the runtime would need to change

Not for this issue. Two cases would justify runtime work later:

- **Context edits.** A message added through `session/context/append` has
  no attribution. This matters only if a UI adds user messages that way.
- **State views.** `session/read` and `session/runs/read` return message origins,
  but per-message acceptance times and authenticated authors live on events.
  A consumer that builds a transcript from state views instead of events would
  need those facts on the message view.

The latest-message display origin in list rows comes from `lastMessageOrigin` in
[P202](p202-session-summaries-and-change-signals.md).

## Tests

- **SDK metadata:** run input takes the acceptance time; steering takes
  the steering time; an item loaded without its admitting event takes its
  own event's time and is corrected when the older page loads; `origin`
  yields the author; a disagreeing `requestedBy` wins.
- **Deferred UI tests:** names appear only with more than one author and not on
  the viewer's own messages; an unknown user ID renders the chosen neutral
  fallback; bot deliveries are unchanged. History integration preserves the
  SDK's fallback correction when older pages load.

## Implemented SDK boundary

`clients/typescript/src/transcript-metadata.ts` exports:

- `transcriptMetadata(sessionId, events)`: a pure projection of the union of loaded
  history and live events into a map keyed by message entry ID. It returns
  `sentAtMs`, `timeSource`, `author` and the full matching `requestedBy` attribution.
  Older pages correct fallback metadata when the caller recomputes; repeated
  entries use their first loaded sequence, not their minimum wall-clock time.
- `transcriptAuthor(origin, requestedBy?)`: the same user-message author rule for
  callers that already have attribution, or for user-role list previews. Actor
  results distinguish authenticated `requestedBy` from display-only `origin`.
  Bot deliveries remain distinct; non-actor requesters are never named as people.

Names, membership lookup, formatting, multi-author display, demo changes and web
transcript integration remain outside this implementation. No wire fields,
endpoints or schema migrations were added. Tests cover queued input, steering,
partial/reordered/overlapping history, repeated context entries, clock regression,
source-less entries, assistant messages, bot deliveries, non-actor attribution,
session isolation and input immutability.

The [integration guide](../documentation/integrating-and-extending/api-and-typescript.md#build-transcript-authors-and-times)
also documents the existing session summaries, resource watch, transcript tail,
Platform identity boundary and workspace snapshot behavior for the designer.

Validated with SDK type checking, all 109 SDK tests (including 15 new metadata
cases), the SDK build, Platform web type checking, and the documentation checks
and site build. UI integration remains deferred.
