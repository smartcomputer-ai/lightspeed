# P171 — Images and documents from tools

Status: implemented, 2026-09-11.

Implementation notes:

- Slices 1 to 4 landed as designed: `engine::media` (handles, admission,
  sniffing, labels, descriptors), companion entries for native MCP results and
  `read_file` on both filesystem domains, handle announcements in all three
  adapters with the DeepSeek drop rule, `mediaHandle` on content views and
  `media` on tool call views, the web transcript (input bands, tool details,
  `media:` links in replies), and the sub-agent hand-off through supplemental
  context entries on the tool batch resume output: the await materializer
  returns the combined result plus one bounded entry list, and a joined-call
  preparation activity returns entries per promise. Both build them from the
  result envelope's `media` list through one shared function; the reducer only
  validates that supplements are user messages of the batch's promises and
  records them after the right result. It never interprets media.
- Live coverage: adapter round-trips on OpenAI Responses, OpenAI Chat
  Completions, and Anthropic Messages; the native MCP matrix with an image,
  an audio clip, and a PDF resource in one result; and a sub-agent that reads
  a PNG, links it, and hands it to its parent over Temporal.
- Provider finding: Claude Opus 5's refusal classifier (`reasoning_extraction`)
  rejects some tool-media follow-ups, reliably a PDF after a tool round-trip
  and occasionally two images, while Sonnet 5, Haiku 4.5, Opus 4.8, and both
  OpenAI APIs read the same requests. The lowering is shape-correct (raw
  probes of the exact request pass on those models); the Anthropic live suite
  therefore runs the single-image case on the default model and the
  images-plus-PDF case on Sonnet 5. No runtime workaround was added.
- Review follow-ups landed: Anthropic keeps every `tool_result` block ahead of
  the media of parallel calls in one user message; an `await` over several
  promises shares one media budget of eight with an omission note, joined
  calls keep one budget each. Transcript media links use browser object URLs
  so images and documents open when clicked; each mounted view releases its
  URL on cleanup, and cached reads are scoped to the media loader.
- Accepted limits: a child resolves the handles it links against its active
  context, so media compacted out of the child before it answers is not
  handed up; awaiting an already-delivered promise again delivers its media
  again, which is the model's own choice; sessions whose histories already
  resumed an await before this change replay a different activity sequence
  and must be closed at deploy (no Temporal patch, by the greenfield stance).

Let images and documents that tools produce reach the model, the same way
user-supplied media already does, and give the model a way to name any media
it has seen so it can reference it in its output. Producers, in this order:
native MCP tool results, `read_file` on both filesystem domains, then
sub-agent and other awaited results. Lowering to provider-native blocks is
required and already exists for run input; that path is reused, not changed.
Converting media the providers do not accept natively (audio, video) stays out
of scope.

## Problem

Run input already carries media end to end. The gateway admits jpeg, png,
webp, gif, PDF, and small text documents; the three adapters lower a
user-role message entry with a media type into an Anthropic `image` or
`document` block, a Completions `image_url` or file part, or a Responses
`input_image` or `input_file` part. Compaction reuses the same materialization.

Tool results do not. Native MCP execution strips the base64 out of `image`,
`audio`, and embedded `resource` blocks, stores the bytes in CAS with
containment edges, and replaces them with a `blobRef` in the structured
output. The model-visible text keeps only the text blocks and substitutes
`[MCP image content stored as structured output]` for the rest. Tool results
are then lowered as plain strings by every adapter, so the pixels never reach
the model. `read_file` rejects non-UTF-8 bytes outright, so an image sitting in
a workspace or environment cannot be looked at either.

Nothing in model-visible text names a media asset. The CAS reference exists
only inside structured output the model never sees, so the model cannot point
at an image it was shown, in an answer, in a hand-off to a parent, or later in
a channel reply. A sub-agent's result is a JSON envelope whose `output` is the
child's final assistant text, so a child that looked at images has no way to
hand them up.

An image-producing MCP server following current client guidance (a text label
before each image, ids and URLs in `structuredContent`) degrades gracefully
here: the agent can drive the workflow but cannot judge the pictures.

## Design

One mechanism serves every producer: **media companion entries**. A tool
invocation already returns a list of model-visible context entries. The
producer appends, after its `ToolResult` entry, one user-role `Message` entry
per admitted media asset, carrying the CAS reference, media type, a
`[image]` or `[document: name]` preview, and an origin naming the tool call.
The entries reuse the exact shape run input produces, so:

- Anthropic pushes the image or document blocks into the same user message
  that holds the `tool_result` block, after it, which is the order Anthropic
  requires. Completions and Responses fold them into one user message
  following the tool message. No adapter change is needed for lowering.
- Compaction, prefix caching, and replay see ordinary entries. The engine
  stays uninvolved: entries are produced by the tool activity, not the
  reducer. No token estimate is attached.
- API projection emits them as `InputItem::Media` items attached to the tool
  call. The projection currently emits `TextRef` for every non-run-input
  entry; it gains the same media branch run input uses.

Order and labelling: the model-visible text refers to each asset by its
position and handle (`[image 1 · media:3f9a2c1d4e7b · image/png · 38 KiB]`,
`[document 1 · media:9b21e04c77a1 · report.pdf]`) and the companion entries
follow in the same order, so the model can tell which is which. A server's
own label text block stays where it was.

### Media handles

Seeing media is one operation; referencing it is another. The second needs a
model-facing name for a content reference, and that name is derived, not
allocated:

```
BlobRef      = sha256:<64 hex>                               # unchanged
ContentRef   = { content_ref, media_type, provider_kind }    # unchanged
MediaHandle  = "media:" + the first 12 hex characters of the BlobRef
media entry  = context Message { role: User } with a media type   # exists
```

Three properties follow. The same bytes always have the same handle. A child
and its parent name an asset identically, so nothing is translated across
sessions. No engine state, counter, or registry is needed, because the handle
is a pure function of the blob.

Every media entry announces its handle in the visible text that introduces
it, whatever its origin. A user upload's `[image]` preview becomes
`[image · media:3f9a2c1d4e7b · image/png]`. An MCP result reads
`[image 1 · media:3f9a2c1d4e7b · 38 KiB]` before its companion entry, and a
`read_file` of a PNG says the same. This is the one rule that tells the model
media exists, and it is origin-independent by construction.

Referencing is a URL scheme in ordinary text. The model writes markdown,
`![the render](media:3f9a2c1d4e7b)` or `[report](media:9b21e04c77a1)`. Output
stays text, the engine stays uninvolved, and the link travels wherever text
travels. Each consumer resolves it its own way: the web transcript maps the
handle to its blob reference and renders through `blobs/read`; a parent
session reads it from a child's output; a channel connector can later turn it
into a photo attachment.

A handle resolves inside a session exactly when a context entry in that
session's log references the blob. That set is a projection over media
entries, not new state; the API's media items gain a `handle` field beside
`blob_ref`, and the web client builds the map from the transcript it already
loads. An unknown handle renders as a broken link. Access is unchanged, since
blob references are already universe-scoped. A link in prose is not a blob
edge for the sweeper, but the companion entry that introduced the asset roots
it in that session, and a parent's companion entry roots it there.

### Never fail a run over tool-produced media

Run input is deliberate, so rejecting it with a clear error is right. Tool
output is not: an agent that calls the wrong MCP tool or reads a binary file
by mistake must be told, not killed. Two rules follow.

- **At tool time**, an asset that fails admission is dropped with a note in
  the visible text: `[image 2 omitted: image/svg+xml is not supported]`,
  `[image 3 omitted: 14 MB exceeds the 10 MB limit]`,
  `[audio omitted: not supported]`. The rest of the result is unaffected.
  Admission mirrors the gateway's run-input rules per asset: jpeg, png, webp,
  gif and PDF up to 10 MB each, at most eight media entries per result.
  Nothing is resized or converted; providers downscale oversized images
  themselves.
- **At request time**, a text-only dialect (DeepSeek Completions today)
  rejects run-input media exactly as now, but drops tool-sourced media entries
  and materializes a text block in their place:
  `[image omitted: this model accepts text only]`. The entry's source
  (`Tool` versus `RunInput`/`Steering`) is what tells the two apart.

Tool-level errors remain fine: `read_file` on a zip still returns an error
result the agent can read and move on from. Only request failures are
forbidden.

### Native MCP results

For each block in `content`, in order:

| Block | Today | After |
|---|---|---|
| `image` with admitted mime | stored, placeholder | stored, companion image entry |
| `resource` with `blob` and `application/pdf` | stored, placeholder | stored, companion document entry; name from the resource `uri` |
| `resource` with `text` | inline JSON text | unchanged |
| `audio`, video-typed `resource` | stored, placeholder | stored, note (conversion out of scope) |
| `resource_link` | inline JSON text | unchanged (not fetched) |
| anything else, or oversized | placeholder | note with reason |

The structured output blob keeps its `blobRef` fields and containment edges
exactly as today; the companion entries reference the same blobs, so nothing
is stored twice. The MCP transport's 2 MiB `tools/call` response budget
already bounds the total.

Results may carry several images. All admitted ones become entries, up to the
per-result cap.

### `read_file`

`read_file` is one operation over the filesystem boundary with a VFS backend
and an environment backend, exposed as `read_file` on the canonical surface
and `Read` on the Claude-Code-like surface. It changes in one place:

- After the ranged read, sniff the leading bytes. PNG, JPEG, GIF, WEBP, and
  `%PDF` are media; everything else follows the current text path, and
  non-UTF-8 bytes still fail as invalid data.
- A media file returns a short text result (path, media type, byte size) and
  one companion entry carrying the bytes. `offset` and `limit` are ignored for
  media files and the result says so.
- Media files are bounded by the same 10 MB per-asset limit, well below the
  512 MiB text ceiling. An oversized media file returns an error result with
  its true size, like an oversized text file does today.

`read_file` yields at most one media entry per call. Reading several images
means several calls, which is also how the model keeps each image labelled.

### Sub-agents and awaited results

Text models produce text; a run's `output` descriptor carries a media type but
is always the assistant's final message. With handles, "returning an image"
is simply a `media:` link in that final text, and no new tool, run fact, or
envelope field is needed:

- When a sub-agent completes, the runtime extracts the `media:` links from
  its final output and resolves each against the child's media entries. The
  result envelope carries the resolved list (`handle`, `content`, `name`)
  beside `output`.
- The parent's `agent_run` result, and `await` on a spawned agent, append one
  companion entry per resolved link after the tool result, labelled
  `[attachment 1 · media:… · image/png]`. Same funnel as MCP, so lowering,
  projection, and the drop rules apply unchanged, and the parent now both
  sees the asset and knows its handle.
- The envelope payload blob records containment edges to those blobs, so they
  survive the child's close until the parent's context entry becomes a root.
- Links the child could not resolve are left as text; the parent sees a
  broken reference rather than a failure.

An explicit `attach` tool, for assets a child wants to return without
mentioning them, is deliberately left out until that case shows up.
Workflow-backed tools resolve promises through the same `PromiseResolution`
payload and could carry the same list; nothing in this slice requires it.

### Web transcript

Two places show media. On a tool call, media items render as image thumbnails
and document chips loaded through `blobs/read`, expandable to full size. In
assistant text, the markdown renderer resolves `media:` links through the
session's handle map, so a `![…](media:…)` image renders inline and a
document link opens the blob.

## Slices

1. Handle derivation; companion-entry construction and admission in the tool
   activity, applied to native MCP results; handle-bearing labels for tool
   and run-input media; projection `handle` field; text-only-dialect drop
   rule in the Completions adapter.
2. `read_file` media sniffing on both backends and both surfaces.
3. Web transcript: media on tool calls and `media:` links in assistant text.
4. Sub-agent hand-off: link extraction at completion, envelope list, parent
   companion entries, containment edges.

## Tests

- Unit: MCP result with two images and a PDF produces three ordered companion
  entries and matching labels; unsupported mime, oversized, and audio assets
  become notes; per-result cap; containment edges unchanged.
- Adapters: a `ToolResult` entry followed by media entries lowers to one
  Anthropic user message with `tool_result` first, and to one user message
  with parts after the tool message on Completions and Responses. DeepSeek
  drops tool-sourced media with a note and still rejects run-input media.
- `read_file`: PNG, JPEG, WEBP, GIF, and PDF fixtures on the in-memory VFS and
  the environment backend; text and non-UTF-8 behaviour unchanged;
  oversized media returns an error result with size.
- Handles: derivation is stable and twelve characters; every media label
  carries one; projection exposes it; a handle the session never saw does not
  resolve.
- Hand-off: `media:` links in a child's final output resolve to its media
  entries, the envelope carries them, the parent's tool result gets companion
  entries in link order, unresolvable links stay text.
- Live (`#[ignore]`), all three providers, Anthropic Messages, OpenAI
  Responses, and OpenAI Completions: an MCP fixture tool returns two labelled
  images and the model is asked to describe each and say which is which;
  `read_file` of a PNG and a PDF and the model reports their content; a
  sub-agent reads an image, links it in its answer, and the parent describes
  it. Plus
  `read_file` of a PNG inside an Incus environment on the Claude-Code-like
  surface.

## Out of scope, noted for later

- Audio and video: anything that needs conversion before a provider accepts it.
- Fetching `resource_link` targets.
- The Anthropic server-side MCP connector path: its `mcp_tool_result` blocks
  replay opaquely, so whatever the connector does with images passes through
  unverified.
- Resolving `media:` links outward to chat channels and bot deliveries.
- Media lists on workflow-backed tool results.
- An explicit `attach` tool.
