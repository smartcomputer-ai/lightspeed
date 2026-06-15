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
- Worker activity: side-effect owner. It reads/writes CAS blobs, invokes
  FFmpeg, calls OpenAI transcription, and maps failures to typed activity
  errors.
- `llm-clients`: low-level OpenAI audio transcription client.
- Deployment/runtime config: owns FFmpeg availability, provider credentials,
  base URLs, timeouts, and feature enablement.

## Transcoding

Use the FFmpeg CLI as an external worker dependency, wrapped by a small Rust
adapter. Avoid libav bindings in the first cut.

Recommended normalization:

1. For `audio/ogg` voice notes, prefer WebM/Opus output:

   ```text
   ffmpeg -hide_banner -nostdin -y -i input.ogg -vn -ac 1 -c:a libopus -b:a 32k output.webm
   ```

2. If WebM/Opus is unavailable or fails, fall back to 16 kHz mono WAV:

   ```text
   ffmpeg -hide_banner -nostdin -y -i input.ogg -vn -ac 1 -ar 16000 -c:a pcm_s16le output.wav
   ```

3. Use `ffprobe` before transcription to enforce duration, codec/container,
   and output-size caps.

Implementation shape:

- `AudioTranscoder` trait;
- `FfmpegAudioTranscoder` implementation using `tokio::process::Command`;
- temp files under a worker-owned temp dir;
- no shell invocation;
- process timeout and cleanup on all paths;
- startup/config check that fails clearly when audio preprocessing is enabled
  but `ffmpeg` is unavailable.

Local development should document `brew install ffmpeg`. Docker/local runtime
images should install the FFmpeg package explicitly.

## Transcription

Use OpenAI's file-oriented audio transcription endpoint first. Initial model
choices:

- `gpt-4o-transcribe` as the quality default;
- `gpt-4o-mini-transcribe` as the lower-cost/default-latency option;
- `whisper-1` only where timestamp features or compatibility require it.

The first cut should request plain JSON/text transcript output. Speaker
diarization, timestamps, streaming transcription, and realtime microphone
sessions are later extensions.

Provider credentials follow the existing model-provider credential path: resolve
inside the activity/runtime layer, never in the bridge, gateway handler, or
engine.

## Failure Semantics

Because the first cut rewrites input before core admission, transcription
failures are admission failures for the submitted `run/start` request, not
completed `RunStatus::Failed` records. Extend admission failure typing as needed
instead of silently dropping audio or submitting an empty text turn.

Failures that should be explicit:

- unsupported audio MIME/container;
- blob missing or over size/duration limit;
- FFmpeg or `ffprobe` unavailable when audio preprocessing is enabled;
- transcode timeout or output over cap;
- provider authentication/configuration failure;
- provider transcription failure.

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

### G1: Gateway And Bridge Admission

- Accept `MediaKind::Audio` in `run/start` with an audio MIME allowlist and
  byte cap.
- Keep `context/append` media rejection unchanged for now.
- Update the TS client contract artifacts if API wire shapes change.
- Teach Telegram and WhatsApp adapters to pass voice/audio messages as media on
  addressed turns; room-event audio remains placeholder text.

### G2: Worker Preprocessing Activity

- Add `WorkflowActivities::preprocess_run_input`.
- Add worker implementation with fake/test adapters.
- Wire preprocessing into `process_admissions` before `drive.admit_command`.
- Add typed admission failure mapping.

### G3: FFmpeg Normalization

- Add `AudioTranscoder` and `FfmpegAudioTranscoder`.
- Add duration/output caps, timeouts, temp cleanup, and startup validation.
- Cover command construction with unit tests; avoid shell execution.

### G4: OpenAI Transcription Client

- Add OpenAI audio transcription support in `llm-clients`.
- Add an activity-level transcriber adapter with provider key resolution.
- Add ignored live tests that fail clearly when credentials or FFmpeg are
  missing.

### G5: End-To-End Voice Note

- WhatsApp voice note live test: submit a spoken question and verify the run
  input contains transcript text and the model answers it.
- Telegram voice/audio equivalent where bot API access is practical.
- Bridge tests for lazy media download and placeholder behavior.

## Acceptance Criteria

- Audio media submitted through `run/start` produces text transcript input
  before the agent plans.
- WhatsApp voice notes work end to end through `interop/messaging`.
- The bridge does not hold OpenAI credentials and does not call FFmpeg.
- The engine has no provider, FFmpeg, or audio-specific side effects.
- Unsupported audio and provider/transcode failures surface as typed admission
  failures.
- Live provider tests are `#[ignore]` and fail clearly when prerequisites are
  missing.
