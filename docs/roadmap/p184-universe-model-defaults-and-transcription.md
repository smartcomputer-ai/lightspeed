# P184 — Universe model defaults and standalone transcription

**Status:** Universe defaults, Platform model settings, standalone transcription,
channel voice preparation, and web dictation implemented, 2026-09-29.
Builds on [CLI session model routing](cli-session-model-routing.md) and
[the runtime CLI](p183-first-class-runtime-cli.md). Supersedes the placement
of transcription inside session admission in
[audio transcription preprocessing](archive/p72-audio-transcription-preprocessing.md).

## Outcome

A universe chooses its default agent model and speech-to-text model through
the public runtime API. These choices work for the web, CLI, bots, and direct
API clients, including installations using custom provider endpoints.

Transcription is an independent operation. Channels transcribe voice messages
before delivering prepared input to a bot. Web dictation produces an editable
draft; only the user's ordinary send action submits it to a session.

The design stays small: two default slots, shared provider resolution, and one
typed transcription workflow. Future capabilities, such as image generation,
can add a slot, adapter, and operation without extending session orchestration.

## Baseline before implementation

- `temporal-server/src/config.rs` resolves an omitted session model from
  `LIGHTSPEED_CHAT_PROVIDER` and `LIGHTSPEED_CHAT_MODEL`, with built-in defaults
  of `openai` and `gpt-5.5`. The API kind is fixed to OpenAI Responses.
- Session creation, configuration replacement, and profile application use
  that deployment fallback. Existing sessions pin their provider identity and
  API kind; model changes within the route remain supported.
- Session admission detects audio in run requests and context upserts, awaits
  preprocessing, and admits the rewritten transcript. Steering follows a
  separate path. Audio preparation therefore has inconsistent entry points and
  runs inside the session's admission loop.
- The transcription activity uses the built-in `openai` provider and the audio
  client's `gpt-4o-transcribe` default. It resolves a stored credential but does
  not apply a stored custom endpoint. Provider failures become ordinary failed
  preprocessing outcomes rather than retryable activity failures.
- Provider records and generation clients already support custom endpoints,
  API-kind declarations, and credential resolution. Custom endpoint validation
  currently admits only Responses and Chat Completions protocols.
- The web composer sends text. Its provider-readiness helper considers any
  provider with a configured credential sufficient, independently of the route
  a session will actually use.

## Decisions

### 1. Universe-owned defaults with a focused API

Store a small, typed model-defaults record in runtime PostgreSQL, keyed by
universe and protected by an optimistic revision. Platform reads and changes
this record through the runtime; it does not keep another copy of the policy.

The initial slots are:

| Slot | Selection | When the default is resolved |
| --- | --- | --- |
| `agentRun` | Agent generation route | Creation of a new session without an explicit or profile model |
| `speechToText` | Transcription route | Admission of a new transcription job without an explicit model |

Every selection contains `{ providerId, apiKind, model }`. Endpoint URLs,
headers, and credentials remain in the existing provider configuration.
Slots can be unset independently; a universe may use agent runs without
configuring transcription, or transcription with no default agent model.

Expose:

- `models/defaults/read`: returns the revision, selections, and per-selection
  configuration status. Keep persisted choices distinct from current provider
  diagnostics.
- `models/defaults/put`: sets or clears one named slot, using an expected
  revision. Clearing uses `null`; unknown slots and incompatible API kinds are
  rejected. An update must not overwrite another slot inadvertently.

Defaults use the existing model-resource policy: members can inspect them;
Operators and Admins can configure them through Platform. Direct runtime keys
continue to use method-group authority. Define these rules in the method
manifest and regenerate its consumers.

A missing default fails with a typed `model_default_unset` error identifying
the slot, but only when the request needs that default. A valid explicit model
does not require a configured default. Invalid or unavailable selections never
silently switch to another provider or model.

### 2. Resolve agent defaults at session creation

New-session precedence is:

1. Explicit session model.
2. Model supplied by the selected profile.
3. Universe `agentRun` default.

Persist the resulting complete model selection in the session. Run precedence
remains explicit run override, then stored session model. Changing or clearing
a universe default does not change an existing session or an admitted run.
Provider identity and API kind remain pinned for the session's lifetime.

Use the same resolution boundary for web, CLI, bot, and sub-agent creation.
Preserve established profile and inheritance rules around that boundary.
Reopening an existing session recovers its stored configuration before reading
mutable defaults. Clones and forks retain the configuration they already copy
or inherit; they do not acquire a new default as a side effect.

For existing sessions, an omitted model in `session/config/put` or an applied
profile means **preserve the current model**. Make this an explicit contract
rule and resolve it against the current configuration revision. The remainder
of configuration replacement keeps its existing semantics, including revoking
omitted features. An explicit incompatible provider route remains an error;
bot profile reconciliation retains its existing session-rotation behavior.

Remove `LIGHTSPEED_CHAT_PROVIDER` and `LIGHTSPEED_CHAT_MODEL` as runtime model
fallbacks. Setup and a deliberate upgrade/import step may write the old
effective selection into specific universe records. Do not populate every
universe with an assumed OpenAI route or overwrite configured values during
startup. Retired variables should produce an actionable configuration message.
Existing deployment credential fallbacks are a separate concern and retain
their current provider-resolution policy.

The development launcher explicitly seeds defaults for its development
universes through the API, without resetting subsequent user choices. The CLI
must support reading, setting, and clearing defaults so setup stays usable
without Platform. Adding a provider in the web UI offers selection of a
default; adding a credential alone does not silently choose a model.

### 3. Separate purpose, protocol, and provider

Purpose names the use within Lightspeed. API kind names the protocol spoken by
an adapter. Provider ID names an endpoint and its authentication configuration.
Several purposes may use the same protocol and still have different defaults.

Keep broader route selection and purpose validation outside the deterministic
engine. The engine's model types continue to describe agent generation only.
Share provider resolution and route validation where useful; provider requests
and responses stay native to their adapters.

The first transcription protocol is the existing
`openai:audio-transcriptions`. Support both the built-in provider and configured
compatible endpoints, including explicit credentialless endpoints. Extend
endpoint validation, the resolver's API-kind check, and the audio client's
per-request transport support together. A custom provider must never fall back
to the built-in endpoint or credential when its configuration is missing or
unusable. FFmpeg remains an optional adapter.

Add a purpose filter to model discovery and use it in the relevant pickers.
An endpoint's protocol declaration does not establish that every model it lists
supports that protocol. Use provider metadata or conservative suggestions and
retain manual model entry. Discovery failure is distinct from missing
configuration and must not prevent saving an otherwise valid explicit route.
Transition existing `selectableOnly` consumers deliberately.

### 4. One standalone transcription workflow

Expose a universe-scoped API independent of sessions:

| Method | Behavior |
| --- | --- |
| `transcriptions/start` | Accepts an audio CAS reference with MIME/name, optional model, language/prompt options, and an idempotency key; returns a transcription ID, resolved model, and status |
| `transcriptions/read` | Returns pending/running/succeeded/failed/cancelled status and, on success, transcript reference and text |
| `transcriptions/cancel` | Requests cancellation of an unfinished job; repeated requests are safe |

The gateway admits the request and starts a `TranscriptionWorkflow`. The
workflow owns validation, optional transcoding, provider execution, result
storage, and cancellation. Activities perform all provider, filesystem, and
store I/O. Keep its dependencies and registration separate from session
admission.

Initially host it in the existing server deployment and sessions worker role
and queue. This is a hosting choice, not session-workflow ownership. Channel
controllers start/join the transcription workflow through the workflow
boundary; they do not dispatch its activities onto another role's queue.
A new worker role or independent capacity can be introduced when operationally
needed, without changing the public operation.

Use bounded provider attempts and an overall deadline, with activity and HTTP
timeouts aligned. Classify transient provider failures for durable retry;
configuration, authentication, unsupported input, and other terminal failures
return typed outcomes. Preserve the existing byte/duration admission limits
and optional transcoder behavior, with provider-specific format support in
the adapter. Cancellation must reach in-flight activities; terminal completion
and cancellation races must converge on one recorded outcome.

### 5. Explicit request identity and result lifetime

Temporal owns the admitted request, attribution, pinned model, status, and
result reference. There is no transcription table, schema migration, lease, or
separate retention service.

Within a universe and requester scope, an explicit idempotency key identifies
one request. Matching retries query the original workflow before consulting
current defaults. Changed input or options conflict. The workflow start uses
reject-duplicate identity; concurrent admissions recover the winning request.
The original submitted request preserves whether its model was omitted.
Temporal's namespace history retention bounds this identity guarantee.

Credentials and endpoint configuration are resolved through the provider record
at execution time. Default changes cannot select another model during retry.
An ambiguous upstream response can still result in another billed attempt;
this does not promise exactly-once provider execution.

Audio and transcript content live in ordinary CAS. Workflow history carries
bounded metadata and references: source audio, resolved model, and transcription
options. The result blob contains only plain UTF-8 text. Input admission refreshes ordinary
CAS grace; it does not create a temporary retention root. Unsubmitted content
may be swept after that grace (seven days by default), and a missing completed
result is reported as expired. A delayed caller can upload again with a new key.

Session admission sets the existing `provenance_ref` to the source audio.
Existing session roots retain both transcript and original recording. Bot-event
roots also retain prepared transcript references alongside their audio while
awaiting delivery. These extend existing reference enumeration, not storage
infrastructure.

The `transcriptions` method group permits Contributors to start jobs through
person-level gateways. Asserted actors can only read or cancel their own drafts;
direct universe keys retain their method-group authority. There is no draft
listing. CAS download continues to use the existing universe-scoped blob access
policy. A separate person-level administrator draft browser is outside this slice.

### 6. Prepared input is the session boundary

Session APIs accept ordinary `Text` or `TextRef` input. Both support an optional
`provenanceRef` pointing to a source blob in the same universe. Admission checks
that the source exists and copies the reference into the existing context
`provenance_ref`. Projections preserve it. This generic metadata supports audio,
documents, or other sources without exposing transformation-specific formats to
sessions. The engine receives prepared text and performs no transcription
orchestration. There is no dedicated transcript input or JSON artifact.

After callers migrate, raw audio `Media` is rejected consistently by run
start, context append, and steering, with an actionable typed error directing
callers to transcription. Context append retains its per-entry failure
semantics. Native audio model input, if added later, will be a separate explicit
capability.

Do not build a general ingress coordinator merely to preserve the old
single-call audio convenience. Audit existing clients before cutover. If a
specific compatibility requirement emerges, document its scope and retirement
separately rather than making it part of the target architecture.

For Telegram and WhatsApp, the conversation workflow starts transcription after
channel authorization and media download/upload, before emitting the bot
event. Retain the original message and attach the transcript explicitly so
downstream routing and filters can inspect its words. Spoken text must not
automatically become a channel-management command.

The conversation controller owns ordering, failure reporting, cancellation,
and delivery retries. Stable inbound identity recovers the same transcription
job. A retry after transcription succeeds reuses the result and does not emit
a duplicate event or run. Preserve the conversation's ordering contract when
audio and text arrive together. Connectors stay transport-only and acquire no
model credentials or routing authority.

### 7. Web dictation edits a draft

The interaction is:

```text
Record → stop → upload/transcribe → review and edit → send
```

The microphone uses browser-supported recording formats selected by feature
detection. Recording and transcription never start, queue, or steer a run.
Successful transcription inserts text into the composer while preserving
existing draft content and edits made during the request. Late completion
after cancellation, navigation, draft submission, or universe/session change
must not restore or modify a stale draft. Release microphone resources on all
exit paths; failures preserve the user's existing text and support retry.

The ordinary send controls determine whether reviewed text starts a run,
queues it, or steers it. It is sent as text, without representing the edited
words as the original audio transcript. Unsubmitted recordings and artifacts
use ordinary CAS grace rather than session retention.

Show transcription availability and an actionable reason when unavailable.
Readiness follows the selected transcription route, including providers that
need no credential. For new sessions, check the effective explicit/profile/
universe selection; for existing sessions, check their stored route. A key on
some other provider does not make the selected route ready. Unknown provider
health remains distinct from invalid configuration.

Add matching demo routes, fixtures, and composer behavior so the browser demo
exercises the same draft flow without requiring a real microphone or provider.

## Migration and rollout

Introduce defaults and the standalone operation before removing old audio
admission. Migrate first-party channel and web consumers to prepared input,
then cut over the raw-audio API contract. CLI and direct-client migration
guidance must describe the upload, transcription, and submission steps.

This is a greenfield cutover. Remove session audio preprocessing, its activity
registration, request/result types, admission errors, and compatibility branches.
Channels always prepare audio through standalone transcription before session
admission. No legacy activity or replay adapter is retained.

Existing sessions keep their persisted models. Upgrade tooling imports model
defaults only for explicitly selected universes. Fresh universes remain
unconfigured until setup supplies their choices. Document failures caused by
unset defaults and retired environment variables before release.

Regenerate public API artifacts, TypeScript clients, method roles, and the
workflow integration contract when changed. Keep schema revision metadata
aligned. Update affected product/development documentation with user review;
this roadmap does not authorize unrelated documentation or root README edits.

## Implementation progress

- [x] Add revisioned universe model defaults, APIs, typed failures, purpose
      validation, and CLI read/set/clear commands.
- [x] Apply creation-time default resolution across session entry points;
      preserve omitted models on existing-session changes and retain route pins.
- [x] Remove environment model fallbacks; support explicit universe setup with
      the CLI and seed untouched development universes through the API.
- [x] Add Platform Models settings, per-selection configuration diagnostics,
      and effective-route readiness.
- [x] Extend provider configuration/resolution and the audio client for
      compatible transcription endpoints, including credentialless transport.
- [x] Add the workflow-owned transcription state, start/read/cancel APIs,
      request deduplication, and requester access rules.
- [x] Return plain transcript text and add generic source provenance to text inputs.
- [ ] Add web dictation with draft preview, cancellation, and demo coverage.
- [x] Move channel voice-message preparation before bot-event delivery and
      validate ordering, failure handling, and retries.
- [x] Audit raw-audio clients, implement the workflow-history transition, and
      remove session preprocessing and obsolete compatibility code.
- [x] Regenerate affected contracts, complete scoped checks, and record
      migration and validation results here.

### First slice

Migration `010_model_defaults.sql` adds one row per configured universe, with
an optimistic revision and independent nullable selections. An untouched
universe reads as revision zero. Setting or clearing either slot advances the
shared revision, and stale writes fail without changing either selection.
Clearing retains the row so development setup cannot undo an intentional
clear. No model policy is inferred or backfilled by migration or server startup.

The read/put APIs use the model method group's existing authority, with read
and configure-resource actions respectively. Reads return persisted selections
and the revision; the web combines these with separate provider diagnostics.
The `speechToText` slot can be configured now, but the existing transcription
path will begin consuming it only when standalone transcription is implemented.

Creation merges explicit and profile configuration before consulting the
universe default. An explicit or profile model bypasses the defaults lookup.
A missing required default returns `model_default_unset` with
`modelDefaultSlot`. Existing-session replacement and profile application resolve
omitted models from current session state and guard the resulting write with
that configuration revision. The engine's deterministic behavior is unchanged.

The CLI exposes:

```bash
lightspeed model defaults read --json
lightspeed model defaults set agent-run \
  --provider <provider-id> --api-kind <api-kind> --model <model-name>
lightspeed model defaults clear agent-run
```

Use `speech-to-text` for the other slot. Set and clear accept
`--expected-revision`; otherwise the CLI reads the current revision once before
writing. A conflict is reported without automatically retrying over a newer
choice. `--json` returns the defaults record for each command.

Before starting an upgraded runtime, apply schema revision 10 with
`cargo run -p temporal-server -- migrate`. Remove `LIGHTSPEED_CHAT_PROVIDER`
and `LIGHTSPEED_CHAT_MODEL`; their presence now produces an actionable startup
error. Select each universe explicitly in the CLI and set its intended route.
Existing sessions remain usable with their stored models, and explicit-model
creation remains available before a default is configured.

The development launcher seeds its development universe and, when enabled, the
Platform Test universe after readiness. This fixture chooses the existing
OpenAI development route only at revision zero. Subsequent settings or clears
are preserved, including concurrent changes. Runtime startup itself never
chooses a default. Live-test fixtures now seed their model policy explicitly.

Validation covers API wire schemas, protocol/purpose checks, model precedence,
omission semantics, CLI round trips and conflicts, launcher behavior, generated
TypeScript consumers, and the runtime library suite. PostgreSQL tests use
temporary isolated schemas to check migrations, concurrent first writes,
universe isolation, slot preservation, clears, stale revisions, and deletion.
Public API contracts and TypeScript consumers were regenerated; the workflow
contract exporter produced no contract change.

Follow-up live validation ran seven selected Temporal tests serially against
temporary local databases and a separate test universe. All passed: the fake
session lifecycle, OpenAI default-backed session execution, OpenAI Responses
and Chat Completions tool round trips, an Anthropic Messages tool round trip,
and both profile integration tests. OpenAI calls used `gpt-6-sol`, the model
now explicitly seeded by the development launcher. Each temporary database
was migrated to revision 10 and removed after testing.

The OpenAI session test now verifies creation from the universe default,
clearing through the API, the typed missing-default failure for new sessions,
and preservation of the original model across reopening, configuration
replacement, and profile application before a real model call.

OpenAI rejected `gpt-6-sol` function tools with its default reasoning setting
on Chat Completions. That protocol's tool fixture now explicitly sets
`reasoning_effort: "none"`, and failure diagnostics report the recorded run
error. Responses passed with its default reasoning settings; production route
selection and generation policy were not changed to hide the provider error.
The slow live suite remains unrun.

### Web defaults slice

Setup → Models (`/u/:slug/models`) now places Defaults below Providers. The
Agent runs selection supports choosing a discovered model, entering a manual
provider/API/model route, and explicitly clearing the slot. Operators and
universe admins can edit; contributors and viewers can inspect. Platform
admins retain their admin access. The Platform routes use the member-scoped
runtime client and its method permission gate.

Edits retain the revision loaded when the dialog opened. A conflict preserves
the draft and requires an explicit reload and review before another write;
there is no automatic overwrite. Discovery failures leave manual selection
available. Adding a provider offers default selection as a separate action.

New-session creation previews the effective session, profile, or universe
model without copying the preview into the request. An unset universe default
blocks creation only when no model was supplied. Existing-session readiness
uses the session's stored model, including embedded bot sessions. Profile and
bot setup editors label omitted selections as the universe default.

Readiness checks the selected provider and API, distinguishes missing or
disabled credentials from unknown availability, and accepts credentialless
providers and models absent from discovery. This reports configuration status,
not proof that a future model call will succeed. The speech-to-text slot is configurable through the runtime API and CLI; its
settings row will accompany web dictation.

The demo uses the same defaults editor, revision checks, and creation policy,
including bot sessions. Clearing a default leaves existing session models
intact. Tests cover API permissions and typed failures, manual selection
during discovery failure, conflicts, provider setup, effective-model previews,
unset defaults, and demo isolation and preservation. The full web, Platform
server, and TypeScript client suites, workspace typechecks, and production and
demo builds pass. The Models page was also visually checked in the browser.

### Standalone transcription and channel preparation

`TranscriptionWorkflow` lives under `temporal-workflow/src/workflows` and runs
on the sessions role's queue independently of session orchestration. The
start/read/cancel API carries requester identity, stable explicit request keys,
and a pinned model. Activity attempts are bounded to three, with a fifteen-minute
schedule budget and a sixteen-minute workflow deadline. Cancellation waits for
activity acknowledgment before recording the terminal state. Missing results
report expiry after an authoritative CAS metadata check, even when bytes remain
in a process cache.

Provider resolution now accepts the audio-transcriptions protocol, including
custom authenticated and anonymous endpoints. Native requests use the selected
model, language, prompt, URL, and configured headers. Endpoint overrides exclude
deployment credentials and organization/project headers. Provider responses and
transcripts are bounded. Audio limits and the optional transcoder belong to
standalone transcription; the session workflow performs no audio processing.

Channels authorize and prepare attachments before starting/joining standalone
transcription through a short activity bridge. The conversation awaits the
result before emitting a bot event; stable conversation/message/attachment keys
reuse the same job. Filters see the transcript in message text; the original
text is retained separately. Spoken commands are never reclassified as channel
commands. Bot attachment `textRef` becomes ordinary text-reference input with
its source attachment as provenance. Existing bot-event and session roots retain
both text and original audio.

New run, context, and steering input rejects raw audio with guidance to use
`transcriptions/start`, poll `transcriptions/read`, then submit
`{type: "textRef", blobRef: ..., provenanceRef: ...}` or reviewed plain text. The in-tree
raw-audio producer was Channels; CLI chat sends text. Direct API callers must
migrate. The greenfield cleanup removes the legacy preprocessing activity,
session input rewriting, audio-specific admission failures, source-to-transcript
retry matching, and channel version branch. Audio helpers and their tests live
under the standalone transcription implementation.

The subsequent simplification removes `Transcript`, `transcript_input`, the
JSON artifact, and transcript-specific provider rendering. The workflow returns
a plain text blob and source-audio metadata. Reviewed dictation can omit source
provenance; channel delivery supplies it through generic text input.

The plain-text path passed API/projection/bot/model-adapter/workflow tests,
356 server unit tests, affected Rust target checks, TypeScript checks, and client
tests. Live transcription, channel redelivery, and PostgreSQL retention tests
passed together. The expiry fixture uses unique text because identical results
share a CAS blob that another session may legitimately retain.

Cleanup validation passed: API and workflow tests, generated contract checks,
356 server unit tests, all server targets, and TypeScript checks. The three
standalone transcription live tests and the channel voice/redelivery live test
passed again against an isolated database, which was removed afterward.

Validation passed: API/auth/model-runtime/bot/workflow suites and generated
contract checks; server unit tests; TypeScript checks, Platform/web/client tests,
and the web production build. Live tests used an isolated PostgreSQL database
and unique Temporal queues. They cover default pinning across changes,
idempotency conflicts, requester isolation, transcoding, session provenance,
transient retry, acknowledged cancellation, ordinary CAS expiry, bot-event
retention, and channel redelivery with spoken command text. A real OpenAI audio
transcription also passed. Web recording and dictation validation follows below.

Web dictation now records through lazy-loaded `extendable-media-recorder`,
choosing a browser-supported WebM, MP4, or Ogg format. The composer requests
microphone access only on an explicit click, caps recordings at ten minutes
and 25 MiB, and releases tracks on completion, cancellation, errors, and
navigation. Upload/start/read/cancel routes use the member-scoped runtime client;
web admission always uses the universe speech default.

The transcript appends to the latest editable draft and never sends itself.
Retry keeps the recording in memory and rejoins an admitted job after transport
failure. Cancel, send, and navigation prevent late completion from changing a
draft. Reviewed text uses ordinary session input without audio provenance.
The demo uses a clearly labeled sample recording and transcript.

Models → Defaults contains separate agent-run and speech-to-text rows below
Providers. Operators can configure either slot; Contributors can transcribe.
Dictation is disabled until the speech default is set and is also subject to
session input permissions and known provider/browser readiness. OpenAI discovery
maps supported file-transcription families to the audio protocol, bypasses the
agent-only age filter for those routes, and keeps them out of agent pickers.
Custom providers continue to use their declared API kinds and manual choices.

Validation passed: 572 web tests, 162 Platform server tests, 13 Rust model
discovery tests, eight API contract tests, TypeScript checks, and production
and demo builds. Browser checks covered speech suggestions, editable transcript
preview, clearing the default, and mobile layout. A real Chromium recorder
produced WebM audio from a generated audio stream and released its tracks;
physical microphones and Safari were not exercised in this slice.

## Acceptance and validation

Use offline tests with fake provider transports for the default validation
loop. Add focused Temporal/PostgreSQL integration coverage where storage or
workflow boundaries require it; live/credentialed suites remain explicit.

- Defaults are universe-isolated and revision-safe. Explicit and profile
  models take precedence; unset defaults only reject requests that need them.
  Updating defaults leaves existing sessions and admitted jobs unchanged.
- Session creation, reopening, profile application, configuration replacement,
  clones/forks, bot rotation, and sub-agent creation preserve the documented
  model semantics. Include engine replay coverage if deterministic behavior
  or event handling changes.
- Custom transcription routes use their configured endpoint, model, headers,
  and authentication. Anonymous endpoints work; missing/disabled custom
  providers never fall back to OpenAI. Discovery absence does not prohibit
  valid manual selection.
- Matching job retries survive default changes and gateway/workflow restarts;
  conflicting payloads fail. Exercise concurrent workflow admission,
  transient versus terminal failures, deadlines, and cancellation races.
- Unsubmitted content uses ordinary CAS grace. Missing results report expiry;
  session-admitted transcripts and bot events retain their original audio and
  transcript through existing roots. Verify requester isolation for job reads.
- Channel redelivery and delivery failure after successful transcription do
  not repeat admitted work. Voice/text ordering and mixed-media failures are
  explicit. Authorization precedes model work, and transcripts are available
  to downstream routing without invoking channel-control commands.
- Web recording/transcription never sends a message automatically. Test
  preserved edits, cancellation, late completion after send/navigation,
  microphone cleanup, permission/format failures, and unavailable defaults.
- Run start, context append, and steering consistently enforce prepared input.
  Text provenance survives projection and session retention without special
  transcript rendering, artifacts, or legacy transcription activity calls.

## Scope boundary

This delivery implements agent defaults and batch speech-to-text. Image
generation, realtime transcription, diarization, automatic model failover,
cross-request transcript caching, and a general processing-pipeline framework
remain outside it. Shared abstractions cover model selection and provider
resolution; new capabilities get typed operations as their requirements arise.
