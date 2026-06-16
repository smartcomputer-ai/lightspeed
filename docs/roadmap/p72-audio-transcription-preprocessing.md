# P72: Audio Transcription Preprocessing

**Status**
- Proposed.
- Split out of P71 G6 after design discussion. P71 remains the messaging
  channel gateway; P72 owns the channel-neutral audio path.

## Goal

Make voice notes and other bounded audio files first-class session input by
transcribing them in the hosted runtime before normal agent planning.

The product behavior should be simple: a client or bridge uploads an audio blob
and submits it as `InputItem::Media { kind: Audio, ... }`; the agent sees a
plain text transcript with an `[audio transcript]` marker and answers the spoken
request. The raw audio remains an opaque CAS blob outside the deterministic
engine.

## Design Position

Use the existing `AgentSessionWorkflow`, not a separate workflow.

Transcription is part of run admission and must be ordered with the session's
normal queue. A child or sibling workflow would add extra idempotency,
cancellation, and state-coordination surfaces before the product needs them.

First cut:

1. Gateway admits bounded audio media as channel-neutral input.
2. The session workflow decodes `CoreAgentCommand::RequestRun`.
3. If the input contains audio entries, the workflow calls a preprocessing
   activity before `drive.admit_command`.
4. The activity reads raw audio blobs, transcodes when required, calls the
   transcription provider, writes transcript text to CAS, and returns rewritten
   `ContextEntryInput` values.
5. The workflow admits the rewritten `RequestRun`; the core sees ordinary text
   and remains deterministic.

If users need an immediate visible run object for long transcriptions, add an
explicit preprocessing/pending run state later. Do not introduce a separate
workflow just to make audio work.

## Ownership

- `interop/messaging`: channel transport only. Detect Telegram/WhatsApp
  voice/audio messages, lazily download bytes when the message becomes a turn,
  upload with `blob/put`, and submit audio media input. It stays credential-free
  and does not run FFmpeg or call model providers.
- Gateway service: channel-neutral admission policy. Enforce MIME allowlist,
  byte caps, per-run media count, and blob existence. It does not transcode or
  transcribe.
- `AgentSessionWorkflow`: deterministic orchestration. It decides whether a
  `RequestRun` needs preprocessing, records the activity result in workflow
  history, rewrites the command input, and then continues through the existing
  drive path.
- Worker activity: side-effect owner. It reads/writes CAS blobs, calls OpenAI
  transcription, optionally invokes a transcoder when the container is not
  provider-accepted, and maps failures to typed activity errors. It must
  function with no transcoder configured.
- `llm-clients`: low-level OpenAI audio transcription client.
- Deployment/runtime config: owns provider credentials, base URLs, timeouts,
  feature enablement, and *optional* transcoder availability. FFmpeg is never
  a required worker dependency.

## Transcoding

FFmpeg must **not** be a hard worker dependency. The same problem returns for
images, video, and GIFs later, so the worker baseline is "no external media
binaries required," and any transcoder is an optional, capability-gated adapter.

Transcode only when the provider will not accept the input directly.
Telegram and WhatsApp voice notes ship as OGG/Opus, which OpenAI's
transcription endpoint accepts as-is, so the common voice-note path needs **no
transcoding at all** — only a cheap duration/size check before upload. The
first cut should:

1. Validate the audio against an accepted-container/MIME allowlist and the
   duration/size caps. Prefer cheap header/metadata inspection over spawning a
   process; only probe deeper if a cap cannot be enforced from metadata.
2. If the container is provider-accepted, send the original (or CAS) bytes
   straight to transcription. No transcode step.
3. Only when the container is *not* provider-accepted, attempt a transcode via
   the optional adapter. If no transcoder is configured, this is a typed
   admission failure (see Failure Semantics), not a worker crash.

Optional transcoder shape (when present):

- `AudioTranscoder` trait, with the worker holding `Option<Arc<dyn
  AudioTranscoder>>` — absence is a normal, supported configuration;
- an FFmpeg-CLI implementation (`tokio::process::Command`, no shell
  invocation, worker-owned temp dir, process timeout and cleanup on all paths)
  is the obvious first adapter, but it is opt-in;
- normalization target when transcoding is required: 16 kHz mono
  (`-vn -ac 1 -ar 16000`), Opus or WAV output;
- no startup hard-fail on missing FFmpeg — audio preprocessing stays enabled
  for provider-accepted containers, and only non-accepted containers degrade
  to a typed "transcoder unavailable" admission failure.

Document FFmpeg as an *optional* enhancement (`brew install ffmpeg`, optional
package in runtime images) needed only to widen container support beyond what
the provider accepts natively — not a precondition for voice notes to work.

## Transcription

Use OpenAI's audio transcription endpoint with the **most advanced transcribe
model OpenAI offers** as the single first-cut default; do not build a
multi-model selection surface yet. The API shape follows whatever that model
requires. `llm-clients` has no audio support today, so this is greenfield — add
exactly the one client call the chosen model needs.

The first cut requests plain text/JSON transcript output. Speaker diarization,
timestamps, alternate/cheaper models, streaming transcription, and realtime
microphone sessions are later extensions; revisit the model surface only when a
second model is actually needed.

Provider credentials follow the existing model-provider credential path: resolve
inside the activity/runtime layer, never in the bridge, gateway handler, or
engine.

## Failure Semantics

Preprocessing rewrites input before core admission, so a preprocessing failure
is an **admission failure** for the submitted `run/start` request, not a
completed `RunStatus::Failed` record. This is deliberate and correct: if
transcription fails there is no text turn to admit, so the command literally
cannot be admitted. Inventing a failed *run* would fabricate a run object for a
turn that never entered the session log.

The obligation this creates is on the **bridge and clients**, not the engine:
a rejected `run/start` for a voice note must not look like the message silently
vanished. Requirements:

- extend admission failure typing so each failure below is a distinct,
  machine-readable variant (not an opaque string);
- the bridge surfaces transcription admission failures to the chat as a clear,
  user-facing notice (e.g. "couldn't transcribe that voice note: <reason>"),
  and logs them — never drops the message silently and never submits an empty
  text turn;
- the contract artifacts and TS client expose the new admission failure
  variants so any client (not just this bridge) can react.

Failures that must be explicit and typed:

- unsupported audio MIME/container;
- blob missing or over size/duration limit;
- no transcoder configured for a non-provider-accepted container;
- transcode timeout or output over cap (only when transcoding is attempted);
- provider authentication/configuration failure;
- provider transcription failure.

Group-failure rule: a single `run/start` input may contain several entries
(text, images, multiple audio clips). If **any** audio entry in the group fails
preprocessing, the **whole submission fails** as an admission failure. Partial
admission would drop context the model needs to answer correctly, so the first
cut is all-or-nothing per submission; the bridge reports which entry failed.

Idempotency rule: retried `run/start` with the same submission id should use the
same deterministic workflow history result. The OpenAI call must live in an
activity, not workflow code.

## Data And Audit

Raw audio stays as its original CAS blob. The transcript is a derived CAS text
blob used as the committed run input.

First-cut transcript text format:

```text
[audio transcript: <name-or-audio>]
<transcribed text>
```

If audit or UI needs a durable relation later, add a derived-artifact record
linking `{ raw_audio_ref, normalized_audio_ref?, transcript_ref, model,
duration_ms? }`. Do not add that relation to the reducer until planning or
projection needs it.

## Implementation Slices

### [x] G1: Gateway And Bridge Admission

Completed 2026-06-16:

- Gateway `run/start` accepts bounded audio media for `audio/mpeg`,
  `audio/mp4`, `audio/wav`, `audio/webm`, and `audio/ogg`, with blob
  existence and byte-cap checks.
- `context/append` media rejection remains unchanged.
- Telegram and WhatsApp adapters pass addressed voice/audio messages through
  the existing lazy media download path; unaddressed room-event audio remains
  placeholder text.
- No API wire shape changed, so committed contract artifacts were not
  regenerated.

- Accept `MediaKind::Audio` in `run/start` with an audio MIME allowlist and
  byte cap.
- Keep `context/append` media rejection unchanged for now.
- Update the TS client contract artifacts if API wire shapes change.
- Teach Telegram and WhatsApp adapters to pass voice/audio messages as media on
  addressed turns; room-event audio remains placeholder text.

### G2: Worker Preprocessing Activity

- Add `WorkflowActivities::preprocess_run_input`.
- Add worker implementation with fake/test adapters; the activity must work
  with no transcoder configured.
- Wire preprocessing into `process_admissions` before `drive.admit_command`.
- Add typed, per-variant admission failure mapping, and the group-failure rule
  (any audio entry failing fails the whole submission).

### G3: OpenAI Transcription Client (No-Transcode Path)

- Add OpenAI audio transcription support in `llm-clients` for the most advanced
  transcribe model, with whatever API shape that model requires.
- Add an activity-level transcriber adapter with provider key resolution.
- Send provider-accepted containers (OGG/Opus voice notes) directly, with only
  duration/size validation — no transcoder required for this path.
- Add ignored live tests that fail clearly when credentials are missing.

### G4: Optional Transcoder (Container Widening)

- Add the `AudioTranscoder` trait and an opt-in FFmpeg-CLI implementation;
  the worker holds `Option<Arc<dyn AudioTranscoder>>`.
- Transcode only non-provider-accepted containers; a missing transcoder yields
  a typed "transcoder unavailable" admission failure, not a crash or hard
  startup failure.
- Add duration/output caps, timeouts, and temp cleanup.
- Cover command construction with unit tests; avoid shell execution.

### G5: End-To-End Voice Note

- WhatsApp voice note live test: submit a spoken question and verify the run
  input contains transcript text and the model answers it (no transcoder
  needed for the OGG/Opus path).
- Telegram voice/audio equivalent where bot API access is practical.
- Bridge tests for lazy media download, transcription-failure surfacing to the
  chat, and placeholder behavior.

## Acceptance Criteria

- Audio media submitted through `run/start` produces text transcript input
  before the agent plans.
- WhatsApp voice notes work end to end through `interop/messaging` with **no
  FFmpeg installed on the worker** (provider-accepted container path).
- The bridge does not hold OpenAI credentials and does not call FFmpeg.
- The engine has no provider, FFmpeg, or audio-specific side effects.
- FFmpeg is never a required worker dependency; the worker runs and transcribes
  provider-accepted audio without it.
- Unsupported audio, missing transcoder, and provider/transcode failures
  surface as distinct, typed admission failures, and the bridge reports them to
  the chat rather than dropping the message.
- If any audio entry in a submission fails preprocessing, the whole `run/start`
  submission is rejected (no partial admission).
- Live provider tests are `#[ignore]` and fail clearly when prerequisites are
  missing.
