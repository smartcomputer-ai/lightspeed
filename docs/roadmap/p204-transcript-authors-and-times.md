# P204 — Authors and times on transcript messages

**Status:** Proposed, 2026-11-13. Not started. Web and Platform only; no
runtime change.

Addresses
[#157](https://github.com/smartcomputer-ai/lightspeed/issues/157). The issue
is labelled `area:runtime`; the work described here lands in the web.

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
- **Author on the admitting event.** `runAccepted` and `steeringAccepted`
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

A message entry gains `sentAtMs`:

- For run input, the `observedAtMs` of the run's `runAccepted` event. A
  queued message enters context later than it was sent, and the acceptance
  time is the time the user sent it.
- For steering input, the `observedAtMs` of `steeringAccepted`.
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
- Any other origin, or none, shows no author. Sessions driven by other API
  clients set their own strings.

### Names

- Resolve user ids through the members list, loaded once per universe and
  cached.
- A user id that is not in the list is someone who has left the universe.
  Show a neutral label such as "Former member".
- Show names only where they add information: when the session has more
  than one distinct author, and not on the viewer's own messages.

### Demo backend

The fixtures gain a second author and message times, so the multi-author
rendering can be seen without a deployment.

## When the runtime would need to change

Not for this issue. Two cases would justify runtime work later:

- **Context edits.** A message added through `session/context/append` has
  no attribution. This matters only if a UI adds user messages that way.
- **State views.** `session/read` and `session/runs/read` return messages
  without a time or author, because those live on events. A consumer that
  builds a transcript from state views instead of events would need them on
  the message view.

The latest-message author in list rows comes from `lastMessageOrigin` in
[P202](p202-session-summaries-and-change-signals.md).

## Tests

- **Transcript fold:** run input takes the acceptance time; steering takes
  the steering time; an item loaded without its admitting event takes its
  own event's time and is corrected when the older page loads; `origin`
  yields the author; a disagreeing `requestedBy` wins.
- **View:** names appear only with more than one author and not on the
  viewer's own messages; an unknown user id renders the former-member label;
  bot deliveries are unchanged.
