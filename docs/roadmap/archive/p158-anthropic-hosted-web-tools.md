# P158 — Anthropic Hosted Web Tools

**Status**

- Hosted web tools implemented 2026-09-04.
- Native assistant content simplification implemented and validated 2026-09-05.

## Problem

The session contract already grants web search and fetch independently, but
toolset resolution was asymmetric: search was hard-wired to OpenAI Responses
and fetch was always the local guarded function. Anthropic Messages therefore
rejected the search grant and could not use Anthropic's hosted fetch tool.

Anthropic server tools also introduce two response behaviors that the generic
Messages adapter must preserve. A `pause_turn` response asks the client to
resend the assistant content unchanged and continue the same turn. Search and
fetch citations carry provider-only replay fields while clients need a small,
provider-neutral source view.

## Design

- The internal toolset config mirrors the public capability grant: one shared
  web search config plus a fetch flag. Resolution chooses provider-native or
  locally executed implementations for the selected API kind.
- OpenAI Responses retains hosted `web_search`; its fetch grant retains the
  guarded local `web_fetch` function. Anthropic Messages lowers search to
  `web_search_20250305` and fetch to `web_fetch_20250910`, both as
  provider-hosted native tools with bounded uses. Hosted fetch also caps
  returned content tokens and enables citations.
- Allowed and blocked domains flow from the session contract into both hosted
  search implementations. Anthropic admits only one list at a time, matching
  its native request contract.
- Anthropic `server_tool_use` and server-tool result blocks stay as exact raw
  provider context, in their original order. A run of consecutive text blocks
  becomes one assistant message whose text is their exact concatenation, so a
  cited answer is one message rather than one fragment per cited span. The
  preamble the model writes before a search stays its own message, and the
  search call projects as a tool step between the two.
- Each assistant text run has one message entry holding its exact native block
  array, encrypted indexes included. API projection derives text and citations
  from that same entry; replay reads it unchanged. Fetch citations resolve
  against earlier fetch results in the same turn. Truncated output retains only
  extracted plain text, so discarded tool/thinking state cannot leave native
  replay dependencies behind.
- OpenAI Responses messages retain their exact native item, including refusal
  parts and URL citation annotations. Separate search sources remain provider
  context, rather than being mistaken for citations.
- Run completion carries the final assistant content descriptor (CAS reference,
  media type, provider kind). The same blob serves replay, display, and final
  output. Shared projection supplies full text to API views and subagents; no
  separately rendered activity output or second context entry is persisted.
- The Anthropic runtime handles `pause_turn` inside one generation activity by
  appending each raw assistant response and calling Messages again. Usage and
  committed context are combined across continuations, and the loop is capped
  at eight provider pauses.

## Tests

- Tool builders cover Anthropic search/fetch JSON, domain validation, and
  provider-hosted execution.
- Toolset and session-admission tests cover both Anthropic grants and preserve
  the OpenAI behavior.
- Anthropic adapter tests cover text-run merging with exact block replay,
  server-tool blocks splitting runs in order, fetch results and cited blocks
  replaying in order, truncated cited output replaying as plain text, and
  `pause_turn` continuation with merged usage.
- API projection tests cover citations projected directly from native messages on
  the state and event paths, fetch citation URL resolution within the turn,
  and the hosted search tool step; web transcript tests cover source display.
- OpenAI Responses adapter coverage verifies exact cited-item replay, safe
  plain-text replay after truncation, and a credentialed hosted-search response with
  real URL citations.
- Ignored direct-provider live tests force each hosted tool independently and
  assert real server-use, server-result, cited-text, and request-version data.

## Acceptance

- A session using `anthropic:messages` may enable search, fetch, or both without
  adding a client-executed web tool.
- Search filters reach Anthropic's native tool request and invalid mixed filter
  lists fail before provider I/O.
- Server-tool state and citation metadata survive durable replay exactly.
- A provider `pause_turn` completes transparently or fails at the bounded
  continuation limit instead of prematurely ending the engine turn.

## Native assistant content and run output simplification

Assistant text runs have one semantic message entry
whose CAS payload contains the original provider JSON, including citations.
OpenAI Responses messages follow the same rule. Adjacent neutral/raw pairs and
replay suppression are removed. Run completion carries the selected message's
content reference, media type, and provider kind, without decoding in the core
or adding a separately rendered activity output. A shared content projection
provides full text for the public API, transcripts, and subagent result
envelopes. Output references remain durable independently of active context.
Session workflows accept authorized source resolutions; a receiving workflow
handles run-terminal notifications and projects their output before resolving a
parent promise. The old direct notification-to-bare-blob shortcut is removed.

`ContentRef` describes the payload: CAS reference, media type, and provider
encoding. The structural follow-up in P160 embeds it in `ContextEntryInput`
and `ContextEntry`, alongside semantic kind, preview, typed provenance, and
accounting. Committing adds session identity and insertion source. Native item
identity is derived from the native payload outside the core. Outputs reuse the
same payload descriptor without context-specific fields.

Validation covers exact native replay, citation projection, partial-output recovery,
run completion/replay, public/generated contracts, frontend rendering, subagent
completion, and real provider web-tool follow-ups.

Verification on 2026-09-05:

- Workspace Rust tests: 1,760 passed; 192 external tests remain explicitly ignored
  in the ordinary suite.
- Direct-provider live suites: 23 passed across Anthropic Messages and OpenAI
  Responses, including hosted web-tool follow-up replay and compaction. The
  OpenAI compaction test's output budget increased from 160 to 1,024 tokens so
  normal model reasoning does not exhaust a budget unrelated to compaction.
- Temporal live suites: nine run-control, six subagent, and three workflow-tool
  notification tests passed. The joined-subagent case also verifies that a native
  run-completion descriptor projects to the same text as the transcript.
- Workspace checks and Clippy cover all targets and features; Clippy passes with
  warnings denied. The workspace build with all features and formatting checks pass.
- The API and workflow contracts and TypeScript consumers are regenerated.
  `npm run check` passes all 264 tests, typechecks, generated-artifact checks,
  and live/demo builds. Vite still reports its existing large-chunk advisory.

Chat Completions content coverage was added on 2026-09-05. Its assistant answers,
refusals, and compaction summaries use plain-text CAS payloads and bounded previews.
The shared content reader preserves full text, including authored JSON and quoted
strings, and replay does not depend on the preview. Regression coverage includes
long Unicode output, multipart text, refusals, partial output, and summaries.
Seventeen direct-provider live tests passed across OpenAI and DeepSeek, including
structured output, media input, tools, compaction, prompts, and skills. A hosted
Completions tool-loop live test also verifies the completed run's descriptor reads
as the same full text displayed in the transcript.

P160 completes the follow-up: Completions content, refusal, and annotations now
share one native payload; message and reasoning API views expose full text;
audio transcripts use structured content and source-audio provenance. See that
plan for the current format and end-to-end validation.
