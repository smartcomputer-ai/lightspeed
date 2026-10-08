# Shared content references, blob tools, and code output

**Status:** Shared content tools, filesystem integration, and code-mode
`media()` / `file()` helpers implemented, 2026-10-08. This includes environment
references, stored `web_fetch` bodies, and selected-output delivery through the
existing attachment pipeline. Tool contracts are owned by Rust DTOs and JSON
Schemas; helper calls use those same admitted capabilities.

Make immutable content usable throughout an agent session: obtain a reference,
inspect it, read selected bytes, pass it to another tool, save it as a file,
or show it to the model. Ordinary tool calls and code-mode scripts use the
same capabilities. `media()` and `file()` compose the ordinary content tools
and select which admitted assets reach the outer code-tool result.

Related decisions:

- [Code mode](p191-code-mode.md) supplies JavaScript execution, session-owned
  tool calls, and selected output.
- [Explicit file attachments](p189-vfs-file-references.md) supplies immutable
  file descriptors, short handles, answer links, and sub-agent handoff.
- [Tool media](p171-tool-result-media.md) and
  [provider media handling](p186-provider-safe-media-and-context-entry-redaction.md)
  supply native image/PDF inputs and request-time normalization and budgets.
- [VFS–environment transfer](p166-vfs-environment-transfer.md) supplies explicit
  capture/materialize operations and streaming transfer infrastructure. Its
  decision to retain existing workspaces and park their replacement still
  applies. This work introduces no replacement artifact-management system.

## Accepted scope

- CAS remains the immutable byte store. VFS remains a named workspace/snapshot
  view. Environment files remain a separate filesystem domain.
- Full SHA-256 references are authoritative content identities. Existing
  `media:` and `file:` handles remain short aliases over recorded content.
- Preserve the current universe-bound blob store and API authorization model.
  Do not add per-blob ACLs, session ownership checks, or a session blob allowlist.
- Add `blob_info`, `blob_read`, and `blob_put` as core tools available in every
  managed session, independently of code mode and other feature families. Add
  an environment-file reference operation and extend file writers to accept
  existing content references under their existing filesystem grants.
- Reuse VFS references, explicit transfers, attachment descriptors, and blob
  retention infrastructure. Large content moves between hosts by reference and
  streaming transfer, without passing through JavaScript or model context.
- Fix `web_fetch` by storing the fetched response body and returning a usable
  content reference, while preserving extracted text and provenance.
- Finish with code-mode `media()` and `file()` output helpers backed by the
  same admitted content tools and existing session attachment pipeline.
- Do not add a named store, `store()`/`load()`, persistent JS globals, or a new
  key-value persistence layer. Existing VFS JSON files and retained blobs cover
  persistence needs. Sessions without VFS can still use blob references.

Pi's `store`/`load` is small JSON state across executions, committed by its
agent only when the script succeeds; the sandbox itself reports writes for its
host to persist. That is a separate concern from binary storage. Lightspeed
does not adopt it or its success-only transaction semantics. Successful tool
effects keep their existing durability when a later script operation fails.
See [Pi's code-mode documentation](https://pi.dev/docs/latest/codemode#store-values)
and [sandbox store contract](https://github.com/earendil-works/pi/blob/main/packages/codemode/README.md#store).

## Current implementation and gaps

| Area | Already present | Gap to close |
| --- | --- | --- |
| Blob storage | CAS, streaming/range operations, retention, public blob APIs, and always-available `blob_info` / `blob_read` / `blob_put` tools | No additional storage layer needed. |
| References | Shared resolver accepts full refs, recorded media/file handles, and supported producer descriptors; aliases survive context compaction | Lookup currently pages durable event metadata; it does not restore reducer state or load historical tool bodies. |
| VFS reference | `vfs_reference` selects an immutable version without reading its body and returns a common descriptor with verified size | None for this slice. |
| Environment reference | `env_reference` captures one file into CAS without VFS, using streaming and durable transfer receipts | Directories continue to use existing capture tools. |
| File writes | All presentations accept text or `content_ref`; VFS reuses CAS content and environment writes stream exact bytes | Environment reference writes require a daemon supporting the new file-write transfer direction. |
| File reads | Existing text/image/PDF behavior is preserved; media descriptors compose with blob tools and writers | General binary access uses reference creation and bounded blob reads. |
| MCP and jobs | Resolver accepts existing `blobRef` envelopes and `mimeType` / `mediaType` metadata; job artifact paths can use `env_reference` | Remote URIs still require an explicit fetch capability. |
| Web fetch | Exact accepted body bytes are stored and referenced alongside extracted text, checksum, and source provenance | Historical checksums without stored bytes remain unavailable. |
| Code output | `text(value)`, awaitable `media()` / `file()`, JSON return values, retained per-call reports, and explicit media/file selection | Native formats and aggregate budgets remain those of ordinary tool media. |

Relevant implementation boundaries are the
[blob APIs](../../crates/api/src/storage.rs),
[blob store](../../crates/harness/src/storage/blobs.rs),
[file-reference tool](../../crates/tools/src/attachments.rs),
[file reads](../../crates/tools/src/fs/tools/read_file.rs),
[file writes](../../crates/tools/src/fs/tools/write_file.rs),
[transfers](../../crates/tools/src/transfer.rs),
[MCP result adapter](../../crates/temporal-runtime/src/worker/mcp.rs),
[job results](../../crates/tools/src/environment/jobs.rs), and
[web fetch](../../crates/tools/src/web/fetch.rs).

## Content identity and descriptors

Keep these distinctions explicit:

| Form | Meaning |
| --- | --- |
| `sha256:<64 hex>` | Immutable bytes in CAS; no inherent filename, MIME type, or per-blob permissions. |
| `media:<12 hex>` | Existing short alias for recorded media. |
| `file:<24 hex>` | Existing short alias for recorded file attachments. |
| VFS path | A location resolved against an attached workspace or snapshot. A live workspace path can change. |
| Environment path | A location on a specific execution machine; capture is required to obtain independent immutable content. |

A common tool-facing content descriptor reuses existing reference and
attachment types rather than introducing another identity:

```json
{
  "content_ref": "sha256:<64 lowercase hex characters>",
  "byte_len": 12345,
  "media_type": "image/png",
  "name": "plot.png",
  "handle": "media:<12 hex characters>"
}
```

The example uses placeholders. `content_ref` and verified `byte_len` are
required for newly resolved descriptors; descriptive fields are optional when
unknown. Names and media types describe a particular use of the bytes. They
are not global mutable properties of the hash. Keep provenance where it is
already available, without treating it as authority.

Descriptors can also carry the existing `source` navigation object. Names,
MIME types, and provenance travel with supplied descriptors or recorded
attachments. A bare full hash may have no such metadata: it identifies bytes,
not the particular URL or filename through which those bytes were obtained.

Every successful blob operation returns the full canonical `content_ref` in
its structured descriptor. Short handles supplement that identity; they never
replace it in durable results. `handle` is optional and is present only when
the corresponding alias is registered in the session. A full hash does not
automatically create a media or file attachment.

| Tool | Reference returned | Short-handle behavior |
| --- | --- | --- |
| `blob_put` | Full ref, verified size, and available content metadata | Does not mint a file attachment/handle merely because content was stored; an already registered handle may be included. |
| `blob_info` | Resolved descriptor with the full ref | Includes a known handle when available; explicit file-reference presentation registers and returns a `file:` handle. |
| `blob_read` | Canonical descriptor alongside requested content/read metadata | Includes a known handle when available; native-media admission registers the corresponding `media:` handle. A range still identifies the original blob, with offsets/counts describing the selected bytes. |

Full refs work well in code because scripts pass values rather than rewriting
hashes. Short handles remain useful when the model authors a later call or
script. Every content-consuming operation accepts the same reference input:
a full ref, an existing short handle, or a supported descriptor. Calling
`blob_info` first is optional, not a required round trip for every consumer.

Paths and URLs are explicit sources for their corresponding tools; they are
not accepted as ambiguous reference strings. A snapshot ref identifies a
manifest blob, not a particular file's bytes. Select a snapshot file through
`vfs_reference` with its snapshot/path arguments.

Keep existing advertised result envelopes usable. In particular, recognize
the owned `content_ref` and existing MCP/job `blobRef` forms at the shared
boundary without recursively rewriting arbitrary external structured data.
Producer schemas must describe the value actually returned to scripts.

## Resolution, authority, and lifetime

Build one host-side resolver used by blob tools, file writers, reference
tools, and output admission. It resolves known content, verifies availability
and size, and supplies recorded metadata or supported content detection when
necessary. It must not infer a MIME type from a hash or trust a filename as
proof of format.

Short aliases resolve against recorded session content, including user inputs,
tool results, handed-off attachments, and completed calls in the current code
scope. Unknown or ambiguous aliases fail explicitly. Do not perform a global
CAS prefix search or silently choose one of several matches. This session
lookup supplies alias identity and metadata; it is not blob authorization.

Today, the gateway authorizes blob API methods at the universe level and
`PgStore` looks up blobs by `(universe_id, digest)`. Neither the gateway blob
handler nor the store checks whether a session previously received the blob.
There are no per-blob ACLs or session blob ownership checks.

Preserve that behavior for the new tools. A full reference can address any
existing blob in the runtime's bound universe, including one absent from the
session's recorded descriptors. Validate the ref, existence, size, and requested
operation without requiring prior session admission of those bytes. Do not add
a blob permission registry or treat short-handle lookup as an allowlist for
full hashes. Per-blob authorization would be a separate future design.

Existing public API authorization, universe isolation, tool admission, and
budgets still apply. Filesystem operations additionally require their existing
source/destination grants. Code-mode `allowedTools` may restrict script calls;
it does not introduce blob-level permissions. Direct calls and scripts use the
same universe-bound storage semantics.

The parent session remains the tool-admission owner. Prepare alias/metadata
resolution facts in the existing tool execution context and record newly
produced references with the normal completion path before settling the JS
promise. The code worker does not restore the session or access storage directly
from guest code.
Reuse workflow-tool starts/signals and the existing code-tool protocol; add no
feature-specific session transport or cross-role activity dispatch.

The implementation scans durable session event metadata in pages of 1,000 for
content operations. It collects input media and ordinary/code-tool attachment
records, including records removed from active context. It does not read old
tool result bodies. A future derived index can optimize this lookup without
changing alias or access semantics.

Recorded results, attachments, workspace manifests, and execution reports retain
their canonical refs through the existing blob graph. Renew admission grace
before publishing a reused reference and record containment edges before its
durable owner is acknowledged. Short handles alone, or hashes embedded only
inside prose, cannot serve as retention roots.

Resolution must not depend exclusively on active model context. An accepted
reference remains usable while its session/durable owner retains it, including
after compaction, workspace changes, or a later script failure. It does not
remain available forever after owner deletion or retention expiry. Derived
lookup indexes, if needed, must be rebuildable from durable records.

Successful effects retain their references even if JS output selection is
lost. Existing execution reports make those outcomes recoverable through the
blob tools. Preserve uncertainty after a lost receipt; do not retry the whole
script or fabricate successful output selection from completed calls.

## Ordinary blob tools

### Availability

Register `blob_info`, `blob_read`, and `blob_put` in the base managed-session
tool catalog. All three are available with code mode disabled, without VFS or
an environment, in timer-only sessions, and before any attachments arrive.
There is no separate blob feature switch or feature-family dependency rule.
Incoming content and MCP discovery therefore do not change whether these tools
are declared. This deliberately makes creation of bounded immutable content a
baseline session capability and adds three tool definitions to plain text
sessions.

Direct model calls can inspect referenced text/JSON, view supported media,
obtain file links, and create text/JSON artifacts without a filesystem. Their
presentation should make these operations clear; raw byte output requires an
explicit binary read mode. Code mode composes the same tool contracts and
does not gain a separate blob API. Its `allowedTools` may still exclude any
blob tool, including `blob_put`; helpers must honor that restriction.

Storage, request, read, and output budgets apply regardless of caller.
`blob_put` stores new immutable session content; it does not grant permission
to write a workspace or environment. Full refs use the runtime's universe-bound
store; short handles resolve through the session's recorded descriptors.

### `blob_info`

Resolve a reference input and return the common descriptor without returning
the body. Use existing stat and supplied/recorded metadata; metadata inspection
does not read file bytes. Native-media presentation performs bounded format
detection when requested through `blob_read`. Report missing, malformed, or ambiguous
references with typed tool errors. A full ref does not need to have appeared
in the session before this call.

Also provide an explicit file-reference presentation option, with an optional
filename override, that registers a `FileAttachment` and returns its link.
This supplies the ordinary-tool counterpart of `file()` without adding a fifth
blob/reference tool. Metadata inspection alone does not publish an attachment
or introduce native media into context. Define a stable content-derived name
when file presentation is requested without a recorded or supplied name.

### `blob_read`

Read bounded data using an explicit format: text, JSON, bytes, or native media.
The implemented `format` values are `text`, `json`, `bytes`, and `media`.
`max_bytes` defaults to 8 KiB and is capped at 1 MiB; `offset` defaults to zero.
Byte-range reads report the full size, returned offset/count, and continuation
offset or completion. Use the existing range interface; do not silently buffer
an entire large object to satisfy a small request.

Text decoding is strict UTF-8. Specify byte offsets independently of text
character counts. Reads that cut an encoding sequence must have a documented,
lossless policy: exact byte mode always remains available, and text mode must
not introduce replacement characters. JSON decoding requires a complete value
within the read budget; a truncated JSON prefix is not a successful JSON value.
Advertise encoding and truncation in the owned output schema.

Native-media mode prepares a supported image/PDF descriptor and attachment
through the same validation and limits as ordinary file reads. It does not
return a base64 image body to JavaScript. Direct calls can therefore show
referenced media to the model; code-mode calls receive its descriptor and keep
it available for explicit output selection.

### `blob_put`

Store bounded inline content and return its descriptor. Accept explicit,
mutually exclusive text, JSON, and byte inputs; do not guess whether an
ordinary string is literal content, base64, a path, or a reference. UTF-8 is the
text encoding. JSON means the serialized JSON value, not an implicit binary
encoding. Byte representation and encoded-size overhead must be included in
the schema and request budgets.

The implemented inputs are `text`, `json` (including JSON null), or `bytes`
(an integer array from 0 to 255), with optional `name` and `media_type`.
Stored content is capped at 1 MiB. Request and code-mode budgets apply
separately to the JSON transport representation, including byte-array overhead.

Use existing content-addressed storage and deduplication. Publish the full ref
and metadata in a retained normal tool result. Optional name/media-type metadata
does not itself select model output, create a file attachment/short handle, or
create a VFS file. Large existing content should be
passed by reference; this tool is not a way around interpreter or request
limits. Upload streams and multipart assembly are outside the initial tool API.

## Existing producers and filesystem tools

### VFS reference and reads

Preserve `vfs_reference`'s metadata-only selection of one immutable file
version, including its explicit snapshot/path recovery path. Add common size
metadata and align the descriptor contract. Keep its explicit file-attachment
behavior and source navigation metadata.

Keep ordinary file reads line-oriented for text and media-aware for supported
images/PDFs. Their media results should pass directly to content consumers.
There is no need to make every read return a blob or create an attachment:
general binary or oversized files use reference creation followed by bounded
blob reads or reference-based transfer.

### Write from an existing reference

Extend VFS and environment write tool contracts to accept exactly one of
existing inline text or a content reference. Preserve existing text calls and
their provider-specific argument names. Reference mode resolves the source
once, pins the immutable bytes, and applies ordinary destination permissions,
path resolution, size limits, and explicit overwrite behavior.

For VFS, publish the existing blob into the destination manifest and retain
its edges. Do not decode it as text or route it through the JS heap. For an
environment, stream the blob through the existing filesystem/transfer boundary
and verify the completed write. The source filename does not implicitly change
the destination path. Binary bytes are copied exactly.

Preserve established retry/operation identities for effectful publication;
repeated delivery must not silently overwrite unrelated later edits. Reuse
existing capture publication and transfer receipts instead of inventing a
second filesystem workflow system. State resulting overwrite/retry semantics
in the tool descriptions and tests.

The implemented writers overwrite existing files, create missing parents,
and reject directory targets. Environment reference writes use a distinct
`writeFile` transfer direction with the existing chunking and receipt engine;
it preserves existing file permissions. Older daemons reject that direction
instead of interpreting it as a less constrained tree replacement. Existing
text calls and materialize/capture wire forms remain supported.

Hosted transfer operation identities include session, run, turn, batch, call,
and argument identity. Activity redelivery reuses its receipt; a model reusing
the same call ID and arguments in a later run performs a new operation.
Environment capture and file writes use the existing bulk, non-retry-safe
activity policy: failed incomplete transfers are aborted, and the model can
issue a fresh call. Completed receipts remain available for redelivery.

### `env_reference`

Capture exactly one environment file into CAS and return a retained descriptor
and explicit file attachment. This is a host-side transfer, not a metadata-only
VFS lookup: the live source is not already an immutable CAS object. It must
work without a VFS mount and for files that native media readers do not accept.

Reuse capture/streaming primitives for size bounds, stable-file checks, digest
verification, cleanup, and retry receipts. Detect source changes according to
the existing capture policy. A successfully captured reference continues to
identify that version if the environment file is later replaced or removed.
An uncertain/retried operation must not silently capture a different version.
Directories remain the responsibility of existing capture/materialize tools.

`vfs_materialize` and `vfs_capture` keep their current explicit VFS/environment
roles. They remain useful for directories and bulk work; neither establishes
an overlay or implicit synchronization. Environment job artifact paths can be
converted through `env_reference`, using the job's owning environment and the
session's existing access/selection rules.

### MCP and job output

Keep MCP `content`, `structuredContent`, error semantics, and the existing
binary-block `blobRef` conversion. Make admitted binary assets available to the
resolver with their known metadata; do not pretend that a remote resource URI
or HTTP link already names a local blob. Fetching those remains an explicit
granted tool operation.

Binary job output already has CAS references, sizes, and MIME metadata. Reuse
those as content inputs. Preserve output sequence/truncation fields and the
distinction between retained output segments and live artifact paths. Neither
blob reads nor reference tools can restore process bytes previously discarded
by the environment's output-retention limit.

## Fix `web_fetch` references

Persist the exact accepted response-body bytes currently used to calculate its
checksum, before text extraction. Return their retained content descriptor
alongside the current extracted text, source/final URL, HTTP status, content
type, byte count, untrusted marker, and text-truncation flag.

The reference identifies the original response body, which can differ from
extracted text. If an extracted-text reference is later added, label it
separately. `max_chars` limits visible extracted text; it must not shorten the
stored body. Existing response-byte limits still bound downloads, and a body
that exceeds them must not be advertised as a complete successful fetch.

For compatibility, the current `sha256` field may remain as a checksum alias
equal to the new descriptor's full ref. New consumers should use the explicit
content descriptor. Existing historical fetch results cannot be made readable
without their bytes; report a missing blob rather than silently re-fetching a
potentially changed URL. Record proper result-to-body containment edges and
preserve source provenance when the content is read again.

Preserve the current web network policy, redirect checks, supported content
types, and source labeling. Arbitrary binary URL download is a separate
capability extension, not a prerequisite for fixing existing `web_fetch`.
Provider-native fetch results remain provider-owned; this change concerns the
Lightspeed tool implementation and must not fabricate raw response bodies for
providers that did not return them.

## Code-mode output: `media()` and `file()`

Code mode provides two awaitable output helpers:

```javascript
const asset = await tools.blob_info({ ref: "media:<known handle>" });
await media(asset);                    // Native image/PDF input for the model.
await file(asset, { name: "plot.png" }); // A file attachment/link.

// Raw content uses the same ordinary storage capability.
await media({ bytes: pngBytes }, { media_type: "image/png" });
await file({ text: report }, { name: "report.txt", media_type: "text/plain" });
```

Keeping admission asynchronous lets
scripts catch missing-reference, unsupported-format, and size errors before
continuing. They accept a supported reference/descriptor or an explicit inline
source object, avoiding ambiguity between text content and reference strings.
Both return the admitted ordinary-tool descriptor. Options may supply `name`
and `media_type`; file presentation uses the supplied or recorded name and
otherwise provides a deterministic content-derived filename. A producer
descriptor remains a reference even if it also contains text or byte-range
data: passing a `blob_read` result selects its original immutable content.

The helpers are thin wrappers over admitted blob operations plus a private
typed-output emitter. `media()` uses native-media read/admission; `file()` uses
explicit file-reference presentation. Inline sources first use `blob_put`.
These underlying calls follow ordinary session tool scheduling, tracing,
retry, and call budgets. Missing capabilities fail normally. Helpers do not
open paths, fetch URLs, or gain broader storage access. Reference-based output
does not load large bodies into JavaScript.

`text()` remains synchronous and retains JSON-compatible values. A JSON return
value remains a separate final value. A private receipt records ordered text
indices and media/file admission request IDs. The native emitter accepts only
successfully completed helper admissions and accounts for their descriptor bytes
under the existing output budget. Passing an object with a `type`, `kind`, or
`attachments` field to `text()` does not select an asset. Parallel helper calls
take their place when their admission completes and they emit.

Finalization verifies each selection against the session's authoritative
completed tool attachments. The compact result's `output` array interleaves
original text values with the existing attachment envelopes (`{kind, data}`).
Its top-level `attachments` list carries the actual selected assets into joined
completion, provider media lowering, and existing client file/media views.
Provider-native tool results still precede their companion media. Historical
interpreter receipts without the new selection field retain their text-only
meaning.

Tool-produced attachments remain recorded at the session owner. At outer
code-tool completion, materialize only explicitly selected media/file outputs
through the existing attachment and companion-context-entry path. Intermediate
assets stay in execution records. Do not automatically expose every image an
MCP tool or file read returned inside a script.

Native media supports the existing image formats and PDF, subject to existing
content validation, normalization, provider capability handling, and aggregate
budgets. Preserve the original blob for downloads even when the provider sees
a resized derivative. Arbitrary documents can be delivered as files; DOCX,
audio, video, and other formats do not become native model inputs merely
because their bytes are available. Additional conversions/formats are later
ordinary tool capabilities. No `image()` alias or separate `audio()` helper is
needed for this slice.

Selected output follows existing partial-result semantics. Ordinary blob
admission failures reject in JavaScript and can be caught; completed effects and
earlier selected outputs remain. Interpreter byte, call, and outstanding-call
budget violations remain terminal execution errors even if caught by JavaScript.
On script failure, retain any valid selected-output receipt already available.
Finalization applies the existing limits of eight native media items and 128
file attachments per result. An invalid, missing, or over-cap selection adds
`output_errors` with its selection index, kind,
request identity when applicable, and diagnostic message. Valid siblings stay
selected; an otherwise successful report becomes failed. Finalization and late
attachment reads retry transient storage errors without rerunning JavaScript;
unavailable content is explicitly reported rather than silently claiming
delivery. After interpreter loss, distinguish retained tool outcomes from missing
output-selection receipts.
Unawaited helper work follows the existing cancellation policy.

File output registers a usable attachment and its short link; it does not
automatically load the document into model context or publish it outside the
session. Existing successful-final-answer selection controls sub-agent handoff.
Historical client views and attachment links must keep resolving the admitted
original content.

## Crates and contracts

- `harness`: reuse `BlobRef`, media/file descriptors, attachment records, and
  deterministic completion/effect application. Add only provider-neutral facts
  actually needed for admission or durable results. No storage or filesystem I/O.
- `tools`: own shared content argument/result DTOs, derived JSON Schemas,
  reference resolution interfaces, blob tool definitions, file-tool projection,
  and the `web_fetch` body-reference contract.
- Runtime/store adapters: supply recorded alias/metadata facts, execute CAS/file
  operations, preserve metadata/provenance and retention edges, and prepare
  media/file attachments. Reuse existing storage and transfer protocols.
- `codemode`: own only source/helper behavior, bounded JSON/native callbacks,
  and typed selected-output receipts. No session restore or direct store access.
- `temporal-runtime` / `temporal-workflow`: preserve ordinary session-owned
  tool effects and separate code execution. Carry the selected-output contract
  and finalize attachments through the generic workflow-tool result path.
- `llm-runtime` and projections/clients: reuse provider-native media lowering
  and attachment presentation; update only what the new selected-output path
  requires.

The implemented tools own their names, descriptor placement, reference-input
unions, read encodings, file-presentation option, and limits in Rust DTOs and
JSON Schemas. Keep those as the source of truth for schemas and execution.
Selected-output receipts stay within the interpreter and runtime report boundary;
the existing workflow attachment contract carries their admitted results.
Regenerate public API/TypeScript consumers when public wire DTOs change and the
workflow contract when execution receipts or result contracts change. Do not
hand-edit generated artifacts or rewrite historical stored result payloads.

## Delivery plan and acceptance

1. **Shared references and ordinary blob tools.** Finalize owned contracts,
   implement the resolver and core registration of `blob_info`/`blob_read`/`blob_put`.
   Include direct file-reference and native-media presentation, retention,
   bounded reads, and useful errors. Verify use with code mode disabled and
   without other feature families, as well as script allowlist enforcement.
2. **VFS/file integration.** Add write-from-reference across existing write
   presentations and align VFS reference/media descriptors. Reuse VFS blobs and
   host-side environment transfer. Verify metadata, exact bytes, overwrite
   behavior, and ordinary text-call compatibility.
3. **Environment references and producer interoperability.** Add `env_reference`
   independent of VFS, integrate MCP/job descriptors, and exercise capture,
   subsequent writes, and ref reuse after source changes.
4. **Persist `web_fetch` bodies.** Store and retain response bytes, expose their
   descriptor, and preserve extracted text, limits, and source provenance.
5. **Selected media and file output.** Implement awaitable `media()`/`file()`,
   typed receipts, finalization, provider lowering, and client presentation.
   Validate direct-call parity and end-to-end model use.

Implementation progress:

- [x] Shared descriptor/input schemas, full-ref lookup, and session alias resolution.
- [x] Core blob tools, existing universe isolation, limits, and durable retention.
- [x] VFS/environment write-from-reference and existing producer alignment.
- [x] Environment file references without VFS.
- [x] Stored and usable `web_fetch` body references.
- [x] Code-mode media/file helpers and selected-output delivery.
- [x] Shared-content schemas, provider catalog fixtures, roadmap progress, and
  end-to-end tests for the implemented tools.
- [x] Selected-output contracts and existing client attachment integration.
- [x] Final helper validation: unit/schema checks, native integration, workflow
  live tests, actual-model execution, and workspace Clippy.

Implementation validation includes native tool tests, metadata-only VFS tests,
real daemon streaming/receipt tests, provider request fixtures, and live
Temporal/PostgreSQL/object-store flows. The helper validation passes 28 native
interpreter unit tests, nine native live tests, and 15 workflow live tests using
real JavaScript and production workflows with a scripted model. Two actual-model
live tests pass, including a model-authored helper script that presents an image,
identifies its color on continuation, and links the generated file. Selected
images and PDF documents also pass native request-lowering checks for OpenAI
Responses, OpenAI Completions, and Anthropic Messages; file-only documents are
excluded from native model input. Runtime tests cover selection validation,
partial failures, aggregate caps, and transient-storage retries. Tools,
LLM-runtime, and runtime unit suites, provider schema/catalog checks, and
workspace Clippy pass. Environment live tests cover
all three provider presentations, files larger than native-media limits,
read-only grants, exact binary restoration, and fresh captures across runs
that reuse call IDs.

Acceptance tests should cover complete data flows, not just individual shapes:

- All three blob tools are present with no optional features, with timers only,
  and with code mode disabled. Adding attachments does not toggle the catalog.
  Direct calls can create/read a text artifact and obtain its file link; a
  code-mode allowlist can exclude each corresponding script capability.
- Every successful blob result carries the full canonical ref. Optional handles
  resolve to that same content; storing a new blob does not implicitly create
  an attachment. Test reuse of existing aliases and explicit file/media
  presentation, including range reads that preserve the original blob identity.
- A script stores JSON, receives a reference, and a later script reads it; a
  VFS JSON file can serve as a named location when the session has VFS.
- A user-supplied media handle resolves, is copied to VFS or an environment,
  and remains pinned to its original bytes after the destination changes.
- An MCP image or job binary-output ref can be inspected, read, saved, and
  selected for output without copying its entire body through JS.
- A binary environment file is captured without VFS, survives source removal,
  and can be restored from its reference. Test changing files, oversized input,
  transfer interruption, conflicting destinations, and repeated delivery.
- A truncated `web_fetch` text preview still has a readable complete accepted
  response body; raw HTML and extracted text are not confused. Historical
  checksum-only results fail honestly when no body exists.
- Direct tools and code mode use identical universe-bound blob semantics. A
  known full hash in the bound universe works even without a prior session
  descriptor; a blob present only in another universe does not. Short-handle
  lookup remains session-based. Cover malformed refs, unknown/ambiguous aliases,
  missing or collected blobs, forged descriptor metadata, inline storage
  excluded by the script allowlist, and refs created earlier in the same script.
- Exercise empty content, Unicode/range boundaries, binary fallback, JSON parse
  failures, EOF, request/result limits, and aggregate selected-output budgets.
- Verify retention and resolution through script failure, cancellation, lost
  output receipts, compaction, replay, restart, and session cleanup. A blob
  printed only inside prose must not be mistaken for an admitted reference.
- Verify `text()` data cannot select media, unselected assets do not enter model
  context, PDFs differ from file-only documents, and valid partial output is
  retained when another selected item fails.
- Exercise image/PDF native requests and file links on supported adapters,
  including existing media normalization and historical attachment views.
  Include a real-model script that moves a reference through tools and emits
  selected media/file output; serialize credentialed Temporal suites.

Use scoped unit/integration checks during each slice, replay coverage for any
deterministic state change, contract freshness checks when relevant, and the
workspace Clippy gate before a Rust PR. Record completed slices and actual
validation results here as they land.
