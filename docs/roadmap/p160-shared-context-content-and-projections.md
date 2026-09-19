# P160 — Shared Context Content and Projections

Status: implemented and verified, including full message API simplification, 2026-09-05.

## Purpose

Finish the content work in P158 by making the payload descriptor a shared part
of context, not a special output representation. Preserve the model-facing
behavior verified by the existing replay and live tests. Complete the structural
change before changing individual payload formats.

## Structural change

- `ContextEntryInput` and `ContextEntry` contain `content: ContentRef`; remove
  their duplicated blob reference, media type, provider-kind fields and copying
  accessors. Run outputs reuse that same descriptor.
- Remove `provider_item_id` from core context. Native IDs remain in native
  payloads and are derived by public projections where available. The core does
  not interpret or reconstruct provider identity.
- Replace the non-provider uses with `provenance_ref: Option<BlobRef>`: an
  immutable artifact recording the entry's origin or construction. Transcripts
  reference source audio, assembled instructions reference their assembly report,
  and skill activations reference their catalog snapshot.
- Public context views carry the shared content descriptor and typed-reference
  provenance. Regenerate contracts and consumers; update the CLI, web, demo,
  integrations, test fixtures, and content projection paths.
- Keep the engine deterministic. Blob reads, decoding, projection, and provider
  materialization stay in effectful adapters or pure helpers outside the core.
- This is a greenfield replacement: remove the old fields and implementations
  without compatibility aliases, a versioned registry, or generic metadata bags.
  Old persisted events, checkpoints, and Temporal payloads are not compatible.
  Rollout requires fresh session/workflow state or an explicit migration of state
  that must be retained; the refactor does not reset existing state automatically.

## Content cleanups, after structural verification

1. Reasoning: retain native payloads and derive only provider-exposed text or
   summaries for display. Bound previews and return full message/reasoning text
   without exposing encrypted/signature fields or rewriting replay content.
2. Chat Completions: keep assistant content, refusals, and annotations together
   in one authoritative payload. Preserve distinct reasoning/tool-call context
   semantics and exact reasoning/tool replay. Materialize only valid request
   fields; response annotations are retained for display but omitted from input.
   Plain authored text/JSON remains exact. Truncated turns retain safe plain text.
3. Audio transcripts: store structured transcript text and filename, retain the
   source audio as provenance, and render model-facing labels at materialization.
   Remove string-prefix parsing from activation text and preserve append
   idempotency, media limits, and failure behavior.

Compaction and transcription remain intentional content transformations.
Provider-native search/citation preservation from P158 remains intact.

## Verification and completion

- Structural stage: focused Rust checks/tests and replay coverage before payload
  changes. Verify CAS provenance and generated API/workflow contracts.
- Cleanup stage: exact request replay, native metadata retention, full visible text,
  authored JSON/refusal output, safe truncation, structured
  audio rendering and idempotency, and frontend/demo behavior.
- Run the full Cargo workspace suite, formatting, all-target/all-feature checks,
  Clippy with warnings denied, and build. Run `npm run check` after generation.
- Run relevant ignored provider live suites using the authorized local `.env`,
  including OpenAI Responses, Chat Completions, Anthropic, and compatible-provider
  reasoning/tool loops. Serialize Temporal live tests and verify run-output reads,
  subagent propagation, prompt/skill provenance, and audio preprocessing.

Progress:

- [x] Design and sequence recorded.
- [x] Shared content and provenance implemented and verified (688 focused Rust tests; all-target/all-feature workspace check).
- [x] Reasoning projection completed.
- [x] Completions content preservation completed.
- [x] Structured audio transcripts completed.
- [x] Generated consumers, full checks, and live validation completed.

Implementation notes:

- Completions replay now matches reasoning and assistant-output provenance by
  run and turn. Regression coverage passes raw responses through engine context
  construction, checks the complete reconstructed message under the documented
  text/refusal lowering policy, and checks prefix stability after follow-up
  input. Adjacent turns and runs remain separate.

- Removed the old flat context fields and copying accessors. Prompt/skill/audio
  provenance is now a blob reference; native IDs are read from payloads for views.
- Shared text projection serves messages, visible reasoning, transcript
  activation, terminal output, subagents, and channel delivery of long replies.
- Completions separates only reasoning and tool-call fields; the remaining native
  message (including unrecognized metadata) stays together. Request lowering
  preserves the established visible text/refusal shape and excludes response
  metadata. Raw payloads are unchanged by projection.
- `AudioTranscript` is an adapter-side JSON record in `llm-clients`, not a core
  context kind. The old server-local prefix renderer/parser is removed.
- Cross-provider transcript tests, reasoning visibility, native
  Completions metadata/citations, authored JSON, channel delivery, and append
  idempotency/provenance have passed.

Final verification:

- Full Rust workspace: 1,766 tests passed; 192 external tests explicitly ignored
  in the regular run. Formatting, all-target/all-feature check, Clippy with
  warnings denied, and all-feature build passed.
- Regenerated API and workflow contracts and TypeScript consumers. Frontend and
  client checks passed: 265 tests, type checks, and production/demo builds. Vite
  still reports its existing large-bundle advisory.
- 69 live tests passed: OpenAI Responses, OpenAI and DeepSeek Chat Completions,
  Anthropic Messages, cross-provider compaction, prompts and skills, audio
  preprocessing, Temporal sessions, subagents, profiles, and workflow-tool
  terminal notifications. Temporal tests ran serially against the authorized
  local services.
- Reworded the Anthropic prompt and skill fixtures from hidden-marker retrieval
  into capacity planning and migration preparation after provider refusals.
  The prompt test still requires both instruction sources, assembly provenance,
  and the exact calculated answer. The skill test still verifies the matching
  file read, avoids unrelated skills, and checks the resulting instruction.
  Both passed; production refusal handling is unchanged. Formatting, workspace
  checks, and Clippy passed again after these fixture edits.

Expanded live and eval verification:

- Ran the remaining suites: 121 additional distinct live tests passed, bringing
  coverage to 190 of the workspace's 192 ignored external tests. This includes
  all provider-client suites, runtime caching and hosted MCP, PostgreSQL/MinIO
  storage and migrations, bots/channels, environment control and registration,
  native MCP/OAuth, run control, tenancy, workflow tools, and ffmpeg.
- The activity-timeout test was skipped at the user's request. The Incus test
  was attempted but remains blocked: `LIGHTSPEED_INCUS_PROVIDER_CONFIG` is unset
  and no reachable Incus configuration was supplied.
- The prompt eval passed all applicable cases: Responses 12/12, Completions
  12/12, and Anthropic 11/11. The existing provider allowlist excludes the
  apply-patch case from Anthropic; eval prompts and assertions were unchanged.
- Native MCP initially failed because the shell lacked its documented private
  network setting. All five tests passed with
  `LIGHTSPEED_MCP_PRIVATE_NETWORKS=localhost,127.0.0.1,::1`.
- Anthropic refused the repetitive caching fixture. Replaced it with a varied
  warehouse stock table and explicit inventory/delivery-planning requests, and
  added provider failure details to assertions. The final complete caching
  suites passed across all three APIs, including one-hour TTL, native replay,
  tool round trips, and superseding catalogs. Observed cache reuse was 88–100%,
  above the unchanged 80% requirement; no production behavior was modified.
- Platform database migration validation also passed. Formatting, workspace
  all-target/all-feature checks, and Clippy with warnings denied passed after
  the fixture changes.

## Full message API simplification

- Return complete projected text for messages, visible reasoning, and run input
  details, regardless of whether the underlying content is plain text or native
  JSON. Keep tool and catalog payload previews bounded and expand their original
  content through `blobs/read`.
- Add `output` and `outputText` to detailed run views, projected from the durable
  run output descriptor independently of the active context. Workflow terminal
  notifications remain small references; consumers can read the completed run.
- Remove the public `blobs/content/read` operation, its DTOs, routes, and clients.
  Retain the shared text projector internally for API projection and subagents.
- Remove message/reasoning fetch state in the web UI and redundant channel blob
  rereads. Regenerate the API consumers and verify full text, native citations,
  binary output descriptors, and bounded tool expansion.
- Exempt responses containing full messages from the gateway's generic 2 MiB
  response budget, since a smaller page cannot shorten a single full message.
  Keep list budgets, tool previews, and existing raw blob reads unchanged.

Verification: 425 focused Rust tests passed. Generated API/workflow contracts
and TypeScript consumers are current. `npm run check` passed all 268 tests,
type checks, and production/demo builds; the existing Vite large-bundle advisory
remains. Formatting, workspace all-target/all-feature checks, and Clippy with
warnings denied passed. The broader workspace test rerun was stopped at the
user's request; queued live suites and evals were cancelled before starting.
The earlier complete live and eval verification above remains the evidence for
the unchanged provider/runtime behavior.

Review follow-up: run-summary previews now use the shared text projection before
the 512-byte limit, so structured audio inputs show transcript text in run lists
and queued-message displays. A regression test reproduced the raw-JSON leak
before the fix and now covers short transcripts, Unicode truncation, and unchanged
CAS content. All 32 projection tests and scoped Clippy with warnings denied passed;
formatting is clean. Repeated native reads/decodes remain a local optimization
opportunity and do not require another durable content representation.
