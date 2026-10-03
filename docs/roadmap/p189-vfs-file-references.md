# Explicit file attachments in agent answers

Status: implemented.

## Behavior

An agent explicitly obtains an immutable file attachment with `vfs_reference`.
The operation resolves a file in an attached VFS workspace or snapshot without
reading its contents or modifying it. Ordinary reads, writes, edits, patches,
searches, and transfers retain their existing outputs and do not implicitly
create file attachments. In particular, no file-reference header is injected
into those results. A capture exports environment bytes into VFS; the agent
then references the captured file explicitly. Snapshot references remain usable
when capture publication encounters a concurrent workspace change.

Obtaining an attachment makes it available in that session. Passing it to a
parent additionally requires mentioning its link in the successful final
answer. Only referenced, known, unambiguous attachments travel in the result
envelope for both `agent_run` and `agent_spawn`/`await`. Intermediate files stay
with the child. The descriptor travels with the text, so the recipient does
not need access to the child's session or workspace. Nested handoff follows
the same rule.

Media follows the existing behavior: reading or producing an admitted image or
PDF supplies media to the model, and mentioning its `media:` link in the final
answer forwards it. Images are immutable blobs too, and may also have a VFS
path. A file reference to an image does not automatically load it as model
input. Receiving a file attachment preserves a link; receiving media retains
the existing native-media admission rules.

Message presentation follows Markdown for both reference types: `[label](ref)`
renders a text link, while `![description](ref)` displays an image inline and
links it to the blob viewer. Image syntax for a non-image falls back to a file
link. Browser previews do not admit media into model context or change attachment
handoff. Either syntax in a successful final answer can forward the attachment.

## Identity and representation

Full SHA-256 blob references remain the authoritative content identity. Short
handles are model-facing aliases over recorded descriptors, not permissions or
a universe-wide registry. Media keeps its existing `media:` handle. File
attachments use `file:` followed by 24 hexadecimal content-digest characters.
Equal bytes have equal file handles; names and optional sources are descriptive
metadata, not a second content identity. Ambiguous handles must not silently
select different bytes.

Tool and sub-agent outputs carry typed attachments: media descriptors or file
descriptors. File descriptors contain a full blob reference, name, optional
media type, and optional source metadata. A source identifies its kind, record,
and path; VFS-specific resolution belongs in the tools/runtime adapters.
The deterministic engine records attachments on completed calls and forwards
runtime-prepared attachments through workflow results without interpreting
filesystem origins. Attachments have a dedicated field; they are not encoded
as tool effects. Public projections expose that field for clients.

Generic filesystem interfaces and operations remain unaware of attachments.
Explicit reference creation resolves one immutable manifest at the VFS tool
boundary. It requires neither a task-local collector nor changes to generic
read/write implementations. The reference tool's own output tells the model
the handle and how to cite it; unrelated tool output stays unchanged.

## Resolution, access, and lifetime

Platform resolves file links from recorded attachments, including historical
completion pages. The parent records handed-off descriptors in its own result.
Unknown, malformed, or ambiguous references are unavailable. Renames,
overwrites, deletion, and workspace detachment do not retarget an attachment.
Source information is optional navigation context and never controls the bytes
opened by a file link.
References created from attached workspaces record the workspace ID and relative
path. The blob viewer links back to both the conversation and that workspace;
the workspace file link follows the current path while the blob remains the
referenced version. VFS mount metadata stays in the tool context, outside generic
filesystem interfaces, and survives sub-agent handoff.

Canonical references on recorded attachments and result-envelope containment
edges keep the bytes reachable for the receiving session. A digest embedded
only in prose does not establish retention. File descriptors have a separate
bounded budget from native media and never consume image/PDF input capacity.

Blob reads use existing universe-scoped permissions. Source metadata and short
handles grant no access. Environment paths remain live, environment-dependent
locations; publishing an independently usable attachment requires explicit
capture. Environment ID shortening remains a separate projection over attached
machines, with full IDs authoritative and ambiguity rejected.

## Validation and progress

- [x] Agree explicit reference creation and final-answer selection for handoff.
- [x] Remove automatic reference collection and injected output headers.
- [x] Add explicit reference creation for attached files and captured snapshots.
- [x] Persist and project typed attachments separately from tool effects.
- [x] Preserve media behavior and selected file handoff through joined/awaited results.
- [x] Verify immutable identity, retention, access boundaries, ambiguity, and nested handoff.
- [x] Verify original tool outputs and Platform historical link resolution.
- [x] Regenerate contracts and run scoped Rust/web checks, Clippy, and relevant live tests.

Fresh validation for the revised contract:

- Tools: 251 unit tests passed, including metadata-only reference creation and
  unchanged read/write/edit output across all three tool surfaces.
- Engine: 245 unit tests passed, including replay of joined and awaited attachment metadata.
- Temporal server: 364 unit tests passed, one unrelated test ignored. Workflow:
  135 passed. API: 96 passed; API projection: 39 passed; schema artifacts: 8 passed.
- Platform: 785 tests passed with `--maxWorkers=2`; the subsequently added tool
  attachment-view regression also passed. The default parallel suite intermittently
  failed existing workspace editor tests that use fixed 40 ms waits; those tests
  passed in the complete two-worker run. Their timing behavior is unchanged here.
- All TypeScript typechecks, the production web build, documentation checks, and
  exact workspace Clippy with all targets and warnings as errors passed.
- Regenerated the public API, TypeScript consumers, Configurator, and workflow
  contract. The workflow exporter produced no external-contract diff.
- All nine sub-agent live tests passed, including explicit file reference selection
  through both joined and spawned/awaited results with a child-only workspace.
  Existing media handoff passed unchanged. The VFS transfer live test also passed,
  exercising environment capture and large-file workspace publication.

Local live suites use Temporal, PostgreSQL, MinIO, and scripted models. External
model-provider suites were not run.

Workspace-origin follow-up: all 252 tools tests, 36 focused file-link/blob-viewer
tests, and the web typecheck passed. All three live attachment handoff tests
passed, including workspace ID and relative-path assertions for joined and
spawned/awaited results.

Markdown presentation follow-up: all 795 web tests passed with two workers,
including historical image references, text-link behavior for both handle types,
preview cleanup, and non-image fallbacks. All 252 tools tests and the web
typecheck passed. Tool descriptions and reference-result guidance describe both
Markdown forms.
