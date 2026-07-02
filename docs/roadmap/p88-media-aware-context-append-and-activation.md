# P88: Media-Aware Context Append And Activation

**Status**
- Proposed 2026-06-29.
- G1 completed 2026-06-30: `run/start` now uses a tagged source shape,
  `context/append` returns per-entry results, and committed contract/TypeScript
  bindings were regenerated.
- Design clarified 2026-07-02: keep both `run/start source=input` and
  `run/start source=context`; do not add a run-start flag to
  `context/append`. Unification belongs in shared admission/preprocessing and
  durable trigger metadata, not by collapsing the public API to one call shape.
- G2-G6 completed 2026-07-02: `context/append` now admits supported media,
  append audio uses the same preprocessing path as run input, context-triggered
  runs carry durable trigger metadata, the messaging bridge eagerly downloads
  and appends supported group media before activation, and bridge operator docs
  describe the new behavior.
- Review hardening completed 2026-07-02: run admission now uses explicit
  engine source enums instead of parallel input/trigger fields, external
  context edits cannot use the reserved `run.*` key namespace, and bridge
  append failures no longer collapse retryable admission failures into
  terminal unsupported-media drops. No legacy/back-compat decoding is kept:
  pre-P88 session logs are not decodable (breaking event-log change, accepted
  while the project is greenfield).
- Source projection cleanup completed 2026-07-02: `RunView` and
  `RunAccepted` expose source enums instead of top-level trigger keys, and
  context-triggered runs snapshot resolved context entry ids so projections
  attribute runs to immutable entries rather than mutable key slots.
- Builds on **P71 (Messaging Channel Gateway, Delivery Outbox, And Media
  Input)** for channel bindings, room context, media input, and activation
  modes.
- Builds on **P72 (Audio Transcription Preprocessing)** for the
  channel-neutral audio transcription path.
- Supersedes the P71/P72 first-cut limitation that `context/append` rejects
  media and that unaddressed room media is represented only as placeholder text.

## Goal

Move messaging ingestion toward the OpenClaw model:

- allowed inbound chat messages are ingested continuously;
- current supported media types are downloaded, admitted, processed, and
  appended to session context even when they do not trigger a run;
- group activation is evaluated after media preprocessing, so a voice note can
  trigger on the same transcript that will later be model-visible context;
- deferred-triggered runs use the already-appended context instead of
  resubmitting the same message as duplicate run input.

The intent is not to widen media support. P88 makes the current supported
media types work correctly in both `run/start` and `context/append`:

- text;
- image media accepted today by `run/start`;
- document media accepted today by `run/start`;
- audio media accepted today by `run/start`, rewritten to text through the P72
  transcription path.

## Design Position

Use an append-first runtime model with two public run-start forms.

For group/chat ingestion where activation depends on processed append output,
the bridge follows:

```text
channel inbound -> context/append -> activation decision -> run/start
```

`context/append` is the ingestion boundary. It validates media, performs any
required preprocessing, commits model-visible context, and returns enough
processed text for the caller to decide whether to start a run.

That does not mean every client must make two API calls. `run/start
source=input` is the atomic ingest-and-run form for callers that already know a
run should start, such as CLI turns, direct messages, and always-on rooms.
`run/start source=context` is the deferred-trigger form for callers that first
need append results, especially transcript `activationText`.

Do not add a "start a run" option to `context/append`. That would create a
third path, fuse append and run idempotency, and force the server to decide
channel policy questions such as whether a partially failed append batch should
still trigger. Appends ingest. Runs trigger work.

Activation remains a bridge/channel policy concern. The hosted API should not
know Telegram, WhatsApp, mentions, `/ask`, group allowlists, or "wake words".
It should only return channel-neutral processed text and entry metadata.

Runs started by the bridge after a prior append should be triggered from
existing context. They should not submit a synthetic text item or resend media
just to wake the agent. Duplicating the just-appended message in run input is
technical debt: the model would see the same user message twice, run
projections would be misleading, and idempotency would couple two separate
concepts.

## Non-Goals

- No new media kinds. Do not add `MediaKind::Video` in P88.
- No raw video ingestion. Existing video-caption behavior may remain
  caption/text-only until a later video design decides whether to extract
  audio, thumbnails, or provider-native video parts.
- No OCR, image captioning, or document summarization during append. Images
  and documents become model-visible media/document context exactly as
  `run/start` admits them today.
- No channel-specific methods or server-side channel policy.
- No provider calls or filesystem/network side effects inside `engine`.

## API Shape

P88 may make breaking wire changes. The project is still greenfield; the goal
is to make the public API model correct instead of preserving awkward first-cut
compatibility.

### Context Append

Keep `context/append` as the method name, but change its response from
parallel key lists to per-entry results:

```json5
{
  "sessionId": "sess_1",
  "entries": [
    {
      "key": "channel.telegram.msg.123.text",
      "item": {
        "type": "text",
        "text": "[telegram group #ops from Ada]\nhello"
      }
    },
    {
      "key": "channel.telegram.msg.123.media.0",
      "item": {
        "type": "media",
        "kind": "audio",
        "mime": "audio/ogg",
        "blobRef": "sha256:...",
        "name": "voice.ogg"
      }
    }
  ]
}
```

Response:

```json5
{
  "contextRevision": 42,
  "results": [
    {
      "key": "channel.telegram.msg.123.text",
      "status": "applied",
      "entry": { "...": "projected ContextEntryInputView or active item view" },
      "activationText": "[telegram group #ops from Ada]\nhello"
    },
    {
      "key": "channel.telegram.msg.123.media.0",
      "status": "applied",
      "entry": { "...": "rewritten transcript entry" },
      "activationText": "lightspeed can you summarize this thread?"
    }
  ]
}
```

`status` is one of:

- `applied`: a new or changed context entry was committed;
- `unchanged`: the key already pointed at the same effective entry;
- `failed`: this entry could not be admitted or preprocessed.

Request-level validation failures still fail the whole call: invalid session,
invalid or duplicate keys in the request, too many entries, malformed JSON, or
a closed session. Entry-level media/preprocessing failures should be returned
per entry so one bad voice note does not discard unrelated room context in the
same append batch.

`activationText` is channel-neutral text the caller may search for activation:

- text input returns the submitted text;
- audio media returns the raw transcript text, without the
  `[audio transcript: ...]` marker;
- image media returns no activation text;
- PDF and binary documents return no activation text;
- text documents may return no activation text in the first cut to avoid
  accidentally treating uploaded files as wake commands.

The response caps `activationText` at 4096 bytes, truncating at a character
boundary, and sets `activationTextTruncated: true` when capped. The committed
context blob remains authoritative; `activationText` is an activation
convenience, not a second source of model input.

`context/append` must not accept a `startRun` or equivalent option in P88. A
caller that wants append output to affect activation should call
`context/append` first, then `run/start source=context` if policy says to run.
A caller that already knows it wants a run should use `run/start source=input`.

Per-entry append failures should use a shared input-admission vocabulary, not a
context-append-only clone of run-start errors. Request-level shape and session
errors still fail the whole call.

```json5
{
  "key": "channel.telegram.msg.123.media.0",
  "status": "failed",
  "failure": {
    "kind": "transcriptionFailure",
    "message": "audio transcription failed"
  }
}
```

The shared API enum is named `InputAdmissionFailureKind` and covers item-local
media/preprocessing failures:

- `unsupportedMedia`;
- `unsupportedAudioMime`;
- `blobMissing`;
- `blobTooLarge`;
- `audioDurationTooLong`;
- `transcoderUnavailable`;
- `transcodeFailure`;
- `transcriptionFailure`;
- `admissionRejected`.

`admissionRejected` marks engine-level admission rejections; callers may treat
it as retryable, unlike the terminal media failures above.

Do not include generic request errors such as `invalidRequest` in this
per-entry enum. Those remain request-level JSON-RPC/API errors.

### Run Start Source

Refactor `run/start` so a run can be started from either new input items or
already-appended context.

Preferred breaking shape:

```json5
{
  "sessionId": "sess_1",
  "submissionId": "telegram:...",
  "source": {
    "type": "input",
    "items": [
      { "type": "text", "text": "hello" }
    ]
  }
}
```

```json5
{
  "sessionId": "sess_1",
  "submissionId": "telegram:...",
  "source": {
    "type": "context",
    "keys": [
      "channel.telegram.msg.123.text",
      "channel.telegram.msg.123.media.0"
    ]
  }
}
```

`source: "input"` is the atomic ingest-and-run form. The submitted input items
are admitted, preprocessed if needed, materialized as run-scoped context
entries under generated `run.{id}.input.{n}` keys at acceptance, and accepted
as a run in one ordered workflow admission. This is the right API for simple
clients and any channel path that knows before admission that it wants the
agent to run. The `run.*` key namespace is reserved; external `context/append`
calls cannot write into it.

`source: "context"` starts a run without creating duplicate context entries.
The listed keys must be active context keys and become the durable trigger
metadata for the run. They are not a context subset filter; the model still
plans over normal active context. Their purpose is audit, idempotency, client
projection, and avoiding duplicate inbound message materialization.

Both source variants are first-class API forms. Neither should be treated as
legacy. Their implementations should converge underneath: one input admission
pipeline, one preprocessing path, and one durable run acceptance shape with
trigger metadata.

The engine records the run source durably as a typed one-of:
`RunSource::Input { input }` or `RunSource::Context { triggers }`. Context
triggers are resolved at admission into `{ key, entry_id }` pairs, and
projections attribute trigger entries by entry id, so a later re-append under
the same key cannot change what a past run was triggered by. Do not hide this
as a dummy text input such as "triggered by message 123".

## Gateway And Workflow Semantics

`run/start` and `context/append` must share one admission implementation for
`InputItem` conversion:

- same MIME allowlists;
- same byte caps;
- same media count limits where applicable;
- same blob existence checks;
- same text-document UTF-8 validation;
- same preview formatting.

This is the main unification point. The API exposes two ergonomic run-start
forms, but the workflow should not have two separate media admission stacks.
Input preprocessing should operate on `ContextEntryInput` values regardless of
whether they arrived through `run/start source=input` or `context/append`.

Audio preprocessing must be generalized from "run input preprocessing" to
"input preprocessing":

```text
InputItem::Media(kind=Audio) -> raw CAS blob -> transcription activity
  -> transcript CAS text blob -> ContextEntryInput(media_type=text/plain)
```

The deterministic engine receives only the rewritten transcript entry. Raw
audio remains in CAS as an opaque uploaded blob, not active model context.

Workflow-owned preprocessing is still required:

- provider calls, transcoding, and blob reads/writes stay in worker activities;
- workflow history records the activity outcome;
- `engine` only admits deterministic `CoreAgentCommand` values;
- typed admission/preprocessing failures remain machine-readable.

Because append preprocessing can rewrite entries, gateway waiting and
idempotency must not compare only the originally submitted `content_ref`.
The append call should wait for the effective committed entry for each key,
then return that effective result.

Run acceptance should also converge around committed context triggers:

- for `source=context`, validate that every listed key exists and points to an
  active context entry, and require at least one key;
- for `source=input`, assign stable run-scoped context keys to the admitted
  input entries;
- record the typed run source durably for both variants;
- snapshot resolved context entry ids for `source=context` triggers in the
  durable event, while keeping run-start idempotency based on source/config.

## Context Append Ordering

`context/append` should be ordered with `run/start` through the session
workflow, not applied as an out-of-band gateway mutation. The bridge must be
able to rely on this sequence:

```text
append message M
append response says M committed
start run triggered by M context keys
run sees M in active context
```

Decided and implemented: appends are ordered through the session workflow's
admission queue like every other command. The previous idle-session
requirement on `context/append` was dropped; an append arriving while a run
is active is queued behind admission processing and becomes visible to
subsequent turns. `context/append` waits for the effective committed entry
(post-preprocessing) per key, so the response reflects what the model will
actually see.

The bridge still serializes per-conversation work, but ordering correctness no
longer depends on it: clients cannot observe flaky "sometimes the run saw the
appended message" behavior.

## Bridge Activation Semantics

Refactor `interop/messaging` inbound handling into separate phases:

1. **Access/control gate**
   - Drop self echoes, empty unsupported messages, unauthorized senders, and
     unauthorized groups.
   - Handle owner/control commands (`/activation`, `/status`, etc.) without
     appending them as chat context unless explicitly desired.

2. **Media load**
   - For allowed non-control messages, download current supported media
     eagerly.
   - Upload media blobs with `blob/put`.
   - Keep existing media support boundaries. Do not add video media in P88.

3. **Context append**
   - Append envelope text plus media entries using stable keys derived from
     provider/account/chat/thread/message/media index.
   - Include sender, channel, chat label, timestamp, message id, and reply
     metadata in the text envelope.
   - Treat append failures as visible/logged bridge failures, not silent drops.

4. **Activation**
   - Evaluate activation against:
     - raw adapter facts such as native mention and reply-to-bot;
     - raw message text/caption;
     - `activationText` returned by `context/append`, especially audio
       transcripts.
   - Activation modes:
     - `mention`: group default; run on prefix, configured mention name,
       native mention, reply-to-bot, or transcript match.
     - `always`: run on every allowed group message via `run/start
       source=input`.
     - `silent`: append only; run only on explicit escape trigger/control if
       configured.
     - direct messages: run by default via `run/start source=input`.

5. **Run trigger**
   - Mention-mode group turns append first, then start with `run/start
     source=context` using the appended keys for that inbound message/batch,
     because the activation decision depends on the append result (e.g. a
     voice transcript). They do not resend the same text/media as
     `source=input`.
   - DMs and `always`-mode groups submit with `run/start source=input`: the
     input form is itself the atomic ingest-and-trigger operation, so the
     message is materialized exactly once as run-scoped context.

Only mention-mode group messages need the two-phase append -> activation-check
-> `run/start source=context` flow. Everywhere activation is known before
admission — direct messages and `always` rooms — the bridge uses `run/start
source=input` as the atomic ingest-and-run path.

Voice trigger terms live in bridge configuration, not in `context/append`.
The bridge may use the same `triggerPrefixes`, `mentionNames`, and bot username
against transcript activation text. If product needs voice-only aliases later,
add them to bridge policy as channel configuration; do not make the generic
append API accept "trigger words".

## Media Scope

P88 makes existing media work in both context append and run start.

### Text

Text messages and captions are appended as normal user-message context entries.
The bridge should continue wrapping them in a channel envelope so group
history retains attribution.

### Images

Images are appended as media context entries using the same allowlist and caps
as `run/start`. They are model-visible to providers that support image input.
No OCR or captioning is performed during append, and image-only group messages
do not trigger in `mention` mode unless native mention/reply metadata or text
caption activation also exists.

### Documents

Documents are appended using the same PDF/text-document support as
`run/start`. Text documents are model-visible as documents, not trigger text.
This avoids treating an uploaded document body as if it were a chat command.

### Audio

Audio is the only supported media type with preprocessing side effects.
Appending audio commits the transcript text as context. The append response
returns transcript `activationText`, enabling voice-note mention/keyword
activation.

If transcription fails, the entry result is failed with the existing typed
audio preprocessing failure kind. The bridge should log and optionally notify,
depending on chat policy. It must not submit an empty run.

### Video

Video remains unsupported as media. Existing caption extraction may still
append text. Raw video download/append is deferred to a later roadmap item.

## Idempotency

Idempotency has two layers:

- `context/append` key idempotency: re-sending the same effective entry for a
  key returns `unchanged`; re-sending a changed entry for the same key replaces
  the keyed active context entry.
- `run/start` submission id idempotency: re-sending the same source/config for
  a submission id returns the existing run; reusing a submission id with a
  different source/config is rejected.

For `source=input`, the source digest covers the submitted input items after
normal request canonicalization plus config. For `source=context`, the source
digest covers the trigger keys plus config. The durable accepted-run event may
snapshot resolved refs for audit, but retries should compare the stable source
shape clients submitted.

Do not combine append and run idempotency into a single `context/append`
optional-run operation. If a caller needs append output before deciding, it
uses two calls. If it already knows a run is required, it uses `run/start
source=input`.

For audio, "same effective entry" means the committed transcript entry, not
the raw audio blob input. A retry that replays the same request should not
transcribe again if the existing key already has an effective committed
transcript. If no committed result exists, retry follows normal workflow
activity idempotency.

## Testing Requirements

- Gateway unit tests:
  - `context/append` accepts image, document, and audio media admitted by
    `run/start`;
  - unsupported media failures are per-entry append results;
  - per-entry failures use the shared `InputAdmissionFailureKind` vocabulary;
  - audio append rewrites to text and returns transcript activation text;
  - unchanged audio append returns unchanged without a second transcription;
  - request-level validation still fails the whole call.

- Workflow tests:
  - append preprocessing is ordered before a following context-triggered run;
  - context-triggered run sees the appended entries once, not duplicated as run
    input;
  - input-triggered run and context-triggered run share preprocessing/admission
    behavior for the same supported media;
  - accepted runs durably record trigger context keys for both source variants;
  - preprocessing failures are typed and do not poison later admissions.

- Bridge tests:
  - unmentioned group image/document/audio messages append context and do not
    start a run in `mention` mode;
  - group voice transcript containing a configured mention triggers a run;
  - `always` mode triggers through `run/start source=input` without duplicate
    context entries;
  - `silent` mode appends and does not run except explicit trigger;
  - media download happens for allowed group room events, not only user turns;
  - video media remains unsupported/caption-only.

- Contract tests:
  - regenerate `interop/contract/` after API shape changes;
  - update the TypeScript client types used by `interop/messaging`.

## Implementation Slices

### [x] G1: API Contract Cleanup

- Replace `RunStartParams.input` with a tagged source shape, or an equivalent
  explicit design that separates new input from context-triggered runs.
- Replace `ContextAppendResponse.appliedKeys/unchangedKeys` with per-entry
  append results.
- Add result fields for effective entry projection, status, typed failure, and
  `activationText`.
- Regenerate committed contract artifacts and TypeScript client bindings.

### [x] G2: Shared Input Admission

Completed 2026-07-02: `run/start` and `context/append` share one gateway
`InputItem` conversion path, so append admits the same media as run input and
reports item-local failures with the shared `InputAdmissionFailureKind`.

- Refactor gateway input conversion so `run/start` and `context/append` share
  media validation and `ContextEntryInput` construction.
- Replace context-append-specific failure kinds with a shared
  `InputAdmissionFailureKind` vocabulary for item-local admission and
  preprocessing failures.
- Remove the current `context/append` media rejection.
- Preserve the current supported media allowlists; do not add video.

### [x] G3: Append Preprocessing

Completed 2026-07-02: append audio runs the shared preprocessing path; the
transcript is committed as the effective context entry and returned as
`activationText` (capped at 4096 bytes with `activationTextTruncated`).

- Generalize P72 preprocessing to operate on admitted input entries, including
  audio-bearing context append commands.
- Ensure audio append commits transcript text as the effective context entry.
- Return transcript activation text without requiring the bridge to read CAS.
- Preserve typed audio failure mapping.

### [x] G4: Workflow Ordering And Idempotency

Completed 2026-07-02: appends are ordered through the session workflow
admission queue (the idle-session requirement was dropped), append waiting
compares effective committed entries, and accepted runs durably record a typed
`RunSource` with `{key, entry_id}` context triggers.

- Route append work through the same session workflow ordering discipline as
  run admission.
- Make append waiting compare effective committed entries, not raw submitted
  audio entries.
- Add durable trigger context metadata to run admission and projection for both
  `source=input` and `source=context`.
- Validate that `source=context` references active context keys and contains at
  least one key.

### [x] G5: Messaging Bridge Eager Ingest

Completed 2026-07-02: the bridge downloads media for all allowed non-control
messages, appends before evaluating activation, and starts mention-mode runs
from context keys; retryable append failures are redelivered instead of being
dropped as terminal.

- Change `NormalizedInbound.fetchMedia` from user-turn-only lazy media to
  allowed-message media loading before append.
- Append all allowed non-control group messages as context in every activation
  mode.
- Evaluate activation after append using native facts, raw text, and append
  activation text.
- Start deferred-triggered runs from context keys instead of duplicate input.
- Continue using `run/start source=input` when channel policy knows before
  admission that a run should start.
- Keep Telegram/WhatsApp media support within the current text/image/document
  /audio boundary.

### [x] G6: Documentation And Operator Guidance

Completed 2026-07-02: this document and `interop/messaging/README.md` describe
the implemented eager-ingest, activation, ordering, and failure semantics.

- Update `interop/messaging/README.md` with eager ingest, activation modes,
  voice transcript triggering, and media support boundaries.
- Update `README.md` only if the public runtime/API model changes enough to
  affect the top-level overview.
- Document failure behavior for voice-note transcription in groups and DMs.
