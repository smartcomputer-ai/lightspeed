# P188 — Stable context catalogs and short environment references

**Status:** Implemented, 2026-10-02. Validation recorded below.

## Outcome

Injected catalogs describe available resources and how to use them. Live
selection, lifecycle status, discovery diagnostics, and internal identifiers
must not repeatedly append whole menus to model context.

Existing durable environment IDs and public API identifiers remain valid.
Model-facing environment references become short handles resolved only against
the session's authorized attachments. Full IDs remain accepted.

## Design

### Publication and metadata

Keep immutable catalog text and structured provenance. A keyed catalog upsert
whose title and content are unchanged updates metadata in place through a
recorded context replacement, retaining the entry ID, position, source, and
supersession link. Real text changes retain existing append-and-supersede
behavior. Replay must reconstruct both paths exactly. This keeps skill API
warnings and availability current without appending identical model messages.

### Environment directory

Publish a sorted directory of references, display names, default markers, and
compact access grants. Omit current selection, lifecycle status, and working
directory; environment_read provides these details, including when selection
tools are disabled. Directory ownership and invalidation depend on the
environment feature, not the selected machine. Environment skill and prompt
sources remain bound to their actual environment.

### Skills

Both environment and VFS skill menus contain names, descriptions, and SKILL.md
paths. Internal skill IDs and redundant directory paths remain available in
structured metadata but disappear from prompt text. Sort by visible path so
snapshot identity changes do not reorder an unchanged menu.

Environment discovery failures retain the last successful catalog only for the
same environment and configured discovery scope. Persist that scope with the
observation so recovery works after worker restarts. Availability and warnings
remain API metadata; the menu describes paths as discoveries whose current
contents are obtained through file reads. Never retain another environment's
skills or reuse an observation after configured roots, working directory, or
access change. Revoking the attachment discards its advertised paths. Older
observations without scope metadata need one successful discovery before they
can be retained on failure. Repeated failures share a stable API warning;
detailed transport errors remain in runtime logs.

### Other catalogs

VFS mounts retain paths and access and describe workspace versus snapshot
storage without exposing internal IDs or snapshot hashes. Sub-agent revisions
remain provenance; only changes to visible choices, descriptions, availability,
or limits append a new menu. Bot directory publication already deduplicates
unchanged contents. Tool definitions remain independent of environment status.

### Environment handles

Preserve already-short IDs. Long generated IDs use `env:` plus twelve UUID hex
characters; other long IDs use twelve SHA-256 hex characters. Resolve handles
against authorized attachments, reject ambiguity, and continue accepting full
IDs. Use the same representation in catalogs, environment control tool results,
and job result handles. Resolve to canonical IDs before registry lookup, routing,
workflow effects, or authorization. Durable environment records keep canonical
IDs; stored model-facing tool results contain short references. Handle collisions must never select an arbitrary
machine; presentation can fall back to full IDs for colliding attachments.

## Verification and progress

- [x] Metadata-only upserts preserve rendered context and replay correctly.
- [x] Environment directory survives selection and status changes.
- [x] Skill and VFS menus omit opaque IDs and redundant fields.
- [x] Failed discovery preserves only matching observations and API diagnostics.
- [x] Short handles work for environment controls and job references, with full
  ID compatibility and ambiguity rejection.
- [x] Scoped Rust tests and relevant generated-contract checks pass.

Validation:

- Library suites for engine, tools, temporal-workflow, temporal-server,
  llm-runtime, and test-support: 1,190 passed; the existing external ffmpeg
  smoke test remains ignored.
- Discovery regression exercises local mock transport timeouts, incomplete
  scans, recovery, and revoked access. It passes with retained stale menus.
- Metadata replacement replay also rejects edits to catalog text,
  supersession links, and superseded versions.
- Provider tool request fixtures regenerated through an explicit ignored
  updater and verified by the normal parity test.
- Workflow integration contract verification and workspace Clippy with all
  targets and warnings denied; formatting and diff whitespace checks.

No database ID migration or change to the public environment ID format is
required. Historical context continues to render its original stored text.
Existing selection-bound environment directories are replaced on refresh;
subsequent selection changes preserve the directory. Real menu edits continue
to supersede old versions under the existing retention and compaction policy.
