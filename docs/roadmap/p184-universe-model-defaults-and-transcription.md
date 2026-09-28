# P184 — Universe model defaults and standalone transcription

**Status:** First slice implemented, 2026-09-28: universe defaults, session
resolution, CLI configuration, and development seeding. Platform settings,
route readiness, and standalone transcription remain to be implemented.
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

Persist the admitted request identity, attribution, immutable resolved model,
and input/result references in a small transcription record. Temporal owns the
execution lifecycle. This record supports recovery, API lookup, and CAS
retention; it is not a generic job framework or a transcript cache.

Within the caller's universe and ownership scope, an idempotency key identifies
one request. A matching retry rejoins that job; conflicting input or options
produce a conflict. Look up the original admission before resolving current
defaults. Fingerprint the submitted request, including whether its model was
omitted, rather than recomputing its identity from today's default.

Persist the resolved route once. Credentials and endpoint configuration are
resolved through the provider record at execution time, following the same
policy as generation. Changes to defaults cannot select another model during
retry. A recoverable workflow-start boundary must handle crashes between
record creation and Temporal start.

Do not use an audio-content hash as the public job identity. Explicitly
repeating a transcription can be intentional, and identical audio may belong
to different callers. Reuse an already persisted result on retry; do not claim
exactly-once upstream billing after an ambiguous provider response.

Audio and transcript content live in CAS; workflow payloads carry bounded
metadata and references. A transcript artifact records text, source audio,
resolved model, and relevant options. Retain provider-native response material
separately if needed, without embedding credentials or transport headers.

Active jobs root their required blobs. Completed jobs retain results and their
idempotency records for an explicit bounded interval, exposed through expiry
metadata. Define and test that interval before delivery. Expired results must
be distinguishable from provider failure. Once a transcript is admitted to a
session, session retention roots its content and source-audio provenance
independently of job expiry. Extend CAS reference traversal accordingly.

Add a dedicated `transcriptions` method group. Platform Contributors can start
jobs; reads and cancellation respect requester ownership and administrator
access, including audio/result download paths. Dictation drafts are not exposed
as a universe-wide job listing. Preserve the existing distinction between
Platform person-level access and direct universe-key authority; a CAS hash is
not a substitute for an access check.

### 6. Prepared input is the session boundary

The target session APIs accept ordinary text or an explicit transcript
reference for transcribed speech. Add `InputItem::Transcript { transcriptRef }`
to materialize the existing transcript context representation and source-audio
provenance. Validate and retain the artifact at admission. The engine receives
prepared content and references and performs no transcription orchestration.

After callers migrate, raw audio `Media` is rejected consistently by run
start, context append, and steering, with an actionable typed error directing
callers to transcription. Context append retains its per-entry failure
semantics. Existing transcript history remains readable. Native audio model
input, if added later, will be a separate explicit capability.

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
words as the original audio transcript. Keep temporary job retention separate
from the session's history.

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

Treat removal of the old workflow activity calls as a Temporal compatibility
change. Inventory existing histories and pending admissions, then use a
supported workflow-versioning or drain/transition strategy with replay
coverage. Keep historical decoding and any required legacy activity handlers
until that transition is complete. Deleting PostgreSQL data is not a rollout
strategy and does not resolve Temporal histories.

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
- [ ] Extend provider configuration/resolution and the audio client for
      compatible transcription endpoints, including credentialless transport.
- [ ] Add the transcription record, workflow, start/read/cancel APIs, request
      deduplication, access rules, and CAS lifetime handling.
- [ ] Add transcript artifacts and prepared transcript input with provenance.
- [ ] Add web dictation with draft preview, cancellation, and demo coverage.
- [ ] Move channel voice-message preparation before bot-event delivery and
      validate ordering, failure handling, and retries.
- [ ] Audit raw-audio clients, implement the workflow-history transition, and
      remove session preprocessing from new execution paths.
- [ ] Regenerate affected contracts, complete scoped checks, and record
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
not proof that a future model call will succeed. The speech-to-text slot stays
out of this UI until standalone transcription consumes it.

The demo uses the same defaults editor, revision checks, and creation policy,
including bot sessions. Clearing a default leaves existing session models
intact. Tests cover API permissions and typed failures, manual selection
during discovery failure, conflicts, provider setup, effective-model previews,
unset defaults, and demo isolation and preservation. The full web, Platform
server, and TypeScript client suites, workspace typechecks, and production and
demo builds pass. The Models page was also visually checked in the browser.

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
  conflicting payloads fail. Exercise the record/start crash boundary,
  transient versus terminal failures, deadlines, and cancellation races.
- Active and retained jobs keep blobs alive. Job expiry releases temporary
  roots while session-admitted transcripts retain their source audio. Verify
  access isolation for job reads and content downloads.
- Channel redelivery and delivery failure after successful transcription do
  not repeat admitted work. Voice/text ordering and mixed-media failures are
  explicit. Authorization precedes model work, and transcripts are available
  to downstream routing without invoking channel-control commands.
- Web recording/transcription never sends a message automatically. Test
  preserved edits, cancellation, late completion after send/navigation,
  microphone cleanup, permission/format failures, and unavailable defaults.
- Run start, context append, and steering consistently enforce prepared input.
  Old transcript history remains readable and recorded workflow histories
  remain replayable across the selected rollout strategy.

## Scope boundary

This delivery implements agent defaults and batch speech-to-text. Image
generation, realtime transcription, diarization, automatic model failover,
cross-request transcript caching, and a general processing-pipeline framework
remain outside it. Shared abstractions cover model selection and provider
resolution; new capabilities get typed operations as their requirements arise.
