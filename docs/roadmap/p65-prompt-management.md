# P65: Prompt Management And VFS Instructions

**Status**
- Proposed
- Builds on P62 CAS/VFS, P63 skills, and the current
  `ContextEntryKind::Instructions` context-entry path.
- First scope is session instructions only. Generic developer-message prompts,
  heartbeat overlays, bootstrap flows, and memory retrieval are deferred.

## Goal

Add a prompt management layer that lets Lightspeed assemble effective session
instructions from editable, inspectable sources such as VFS prompt files and
runtime defaults.

The first implementation should publish resolved prompt material as a normal
engine context edit:

```text
instructions.100.prompts.0000.* -> ContextEntryKind::Instructions -> VFS source CAS blob
instructions.100.prompts.0001.* -> ContextEntryKind::Instructions -> VFS source CAS blob
```

The engine remains deterministic. It sees only immutable blob refs and context
events. It does not read files, watch directories, resolve prompt roots, or
assemble prompt text.

This gives Lightspeed:

- an editable prompt surface in VFS workspaces,
- deterministic prompt assembly and source ordering,
- visible prompt provenance, hashes, sizes, and warnings,
- prompt refresh before a new run starts while the session is idle,
- a path to later automatic reload without changing the core engine model.

## Current State

Lightspeed's live instructions path is context-entry based:

- `temporal-workflow` opens a session and upserts default instructions as
  `instructions.000.default`.
- `engine` accepts external instruction context edits only under
  `instructions.*` keys.
- `engine` plans instruction entries first, sorted by key.
- `llm-runtime` materializes OpenAI Responses instruction entries into the
  provider top-level `instructions` field by concatenating their text.
- Skills use a different lane: `SkillCatalog` and `SkillActivation` context
  entries are rendered as OpenAI developer messages.

The older `SessionConfig.context.instructions_ref` design is no longer the
right narrow waist. Prompt management should use the live context-entry path.

## Design Position

Prompt management is a runtime/API/tooling concern that maps VFS prompt source
files to instruction context entries.

Target shape:

```text
VFS mounts / prompt conventions / product defaults
  -> gateway prompt refresh before run
  -> VFS/CAS reads outside engine
  -> pure prompt source resolver/report builder
  -> one instruction entry per published prompt source
  -> CoreAgentCommand::ReplaceContextPrefix {
       key_prefix: "instructions.100.prompts",
       entries: {
         "instructions.100.prompts.0000.project": source_ref,
         "instructions.100.prompts.0001.style": source_ref
       }
     }
  -> existing engine planning and provider materialization
```

The gateway or a future controller owns refresh policy. The engine owns only
validation, ordering, replay, and request planning.

## First Scope

P65 G1 should focus on session instructions:

- publish each prompt file as its own `ContextEntryKind::Instructions` entry,
- use exact source CAS blobs from VFS instead of rewriting prompt text,
- manage entries under the stable prefix `instructions.100.prompts`,
- refresh while idle before `run/start`,
- expose a prompt status/report view,
- keep the existing default instructions entry separate.

Per-file prompt entries match the data model better than a skill-style compiled
catalog. Refresh remains atomic through `ReplaceContextPrefix`: the runtime
submits the complete desired prompt set, and the engine replaces stale, renamed,
or deleted entries under the managed prefix in one deterministic context event.

## Non-Goals

- Do not put VFS reads, host filesystem reads, file watches, or prompt assembly
  in `engine`.
- Do not reintroduce `ContextConfig.instructions_ref`.
- Do not add generic developer-message prompt files in G1.
- Do not force heartbeat, bootstrap, or memory content into permanent session
  instructions.
- Do not make OpenClaw filenames such as `SOUL.md` or `MEMORY.md` engine
  semantics.
- Do not rely on OS file watches as the durable source of truth.
- Do not silently treat untrusted uploaded files as system instructions.

## Prompt Conventions

Use a small convention first, not a full manifest.

For each writable workspace mount, discover prompt roots:

```text
<mount>/.lightspeed/prompts
<mount>/.agents/prompts
```

Within a prompt root, read:

```text
instructions.md
instructions.d/*.md
```

Assembly order:

1. roots sorted by root id/path,
2. `instructions.md`,
3. `instructions.d/*.md` sorted lexicographically.

Missing roots and missing optional files are not errors. Invalid paths,
non-text files, UTF-8 failures, size-limit violations, and read failures are
reported. Required files and explicit ordering can be added later through a
manifest when the product surface needs it.

## Source Entries

Prompt source files are not wrapped or concatenated in G1. The instruction
entry `content_ref` points at the exact VFS file bytes.

Ordering is encoded in the managed context keys:

```text
instructions.100.prompts.0000.<source-id>
instructions.100.prompts.0001.<source-id>
```

The path is provenance, not authority. The report records source path,
workspace/snapshot provenance, writability, source hash, and the context key
used for published files.

## Refresh Semantics

The first refresh policy is simple:

- on `run/start`, load the session state;
- if the session is open and idle, refresh prompts and skills before admitting
  the run;
- if the session has an active or queued run, do not refresh prompts in G1;
- the run uses whatever instruction context was already active.

This matches the practical skill-catalog model and avoids mutating active
context while a request is being planned or executed.

Queued-run caveat: the current gateway can admit queued runs while another run
is active. There is no gateway boundary immediately before a queued run becomes
active. Automatic refresh before each queued run should wait for a workflow
activity/controller design.

## Active Run Behavior

Prompt refresh must not rewrite an in-flight provider request.

If prompt files change during an active run:

- the active run is unaffected,
- G1 does not apply a pending prompt automatically,
- the next `run/start` that observes an idle session refreshes instructions.

Later automatic reload can track pending prompt materializations and apply them
when the session becomes idle.

## Reports And Read API

Every prompt materialization should produce a CAS-backed report.

Suggested shape:

```rust
pub struct PromptInstructionsBuild {
    pub entries: Vec<PromptInstructionEntry>,
    pub report_ref: BlobRef,
    pub report: PromptInstructionsReport,
}

pub struct PromptInstructionsReport {
    pub schema_version: String,
    pub total_chars: u32,
    pub total_bytes: u64,
    pub sources: Vec<PromptSourceReport>,
    pub warnings: Vec<PromptWarning>,
}

pub struct PromptSourceReport {
    pub id: String,
    pub path: String,
    pub published: bool,
    pub context_key: Option<ContextEntryKey>,
    pub workspace_id: Option<VfsWorkspaceId>,
    pub workspace_revision: Option<u64>,
    pub snapshot_ref: Option<BlobRef>,
    pub content_ref: BlobRef,
    pub chars: u32,
    pub bytes: u64,
    pub sha256: String,
    pub truncated: bool,
}
```

The report should be inspectable through a narrow `prompts/active` convenience
API. `session/read` already exposes active context state, so `prompts/active`
should not become a refresh endpoint in G1. It should return active prompt
instruction entries, optional shared `report_ref`, and decoded report JSON when
the report blob is available. Full prompt file text remains available through
`blob/get` for each instruction ref.

## Crate And Module Shape

Keep implementation close to the existing skill/VFS tooling first:

```text
crates/tools/src/prompts/
  mod.rs
  model.rs
  assembler.rs
  vfs.rs
```

Public API should mirror the skill catalog shape:

```rust
build_prompt_instructions(...)
prepare_prompt_instructions_publication(...)
prompt_source_instructions_context_input(...)
conventional_vfs_prompt_root_specs(...)
resolve_mounted_vfs_prompt_roots(...)
```

Do not rename the `tools` crate for G1. Add `tools::prompts` alongside
`tools::skills`. If prompt and skill code start duplicating root resolution,
fingerprinting, or publication helpers, extract a small shared module such as:

```text
crates/tools/src/context_sources/
  vfs_roots.rs
  fingerprints.rs
  publication.rs
```

Avoid premature renaming. The shared abstraction should be based on real code
that both skills and prompts use.

## Shared Infrastructure With Skills

Prompts should reuse the shape of skills, not the skill data model.

Useful shared patterns:

- conventional VFS roots derived from session mounts,
- `MountedVfsFileSystem` for reading mounted snapshots/workspaces,
- deterministic root and source sorting,
- CAS writes for reports and exact source refs from VFS,
- source fingerprints from observed inputs,
- compare current managed context set and emit no-op/prefix-replace commands,
- gateway wait loops that confirm the expected context entry was applied.

Skill-specific pieces should remain separate:

- `SKILL.md` discovery,
- YAML frontmatter parsing,
- skill metadata/catalog schemas,
- activation and developer-message rendering.

Prompt-specific pieces should be separate:

- prompt source discovery,
- prompt source selection,
- instruction report schemas,
- managed instruction context key choice.

## Gateway Integration

Add a shared pre-run refresh helper:

```rust
load_session_state_with_current_run_context(session_id)
refresh_run_context_for_idle_session(session_id)
```

That helper should:

1. load session state,
2. if open and idle, refresh prompt instructions,
3. refresh skill catalog,
4. reload state if either refresh applied a command.

Then `run/start` should call this helper before encoding
`CoreAgentCommand::RequestRun`.

Prompt refresh should:

1. list session VFS mounts,
2. find conventional prompt roots,
3. resolve mounted VFS prompt roots,
4. read existing prompt sources,
5. select publishable prompt sources and write a report to CAS,
6. compare active entries under `instructions.100.prompts`,
7. submit `ReplaceContextPrefix` only when the managed prompt set changed,
8. replace stale entries with the new set or clear the prefix when no prompt
   sources remain.

Skill refresh should keep its current behavior, but `run/start` should invoke
it too. Otherwise catalog freshness depends on whether a skill endpoint was
used before the run.

## Trust And Safety

Prompt files are trusted operator state. Treating workspace files as session
instructions is a privilege boundary.

Rules:

- prompt roots should default to project/operator workspaces, not arbitrary
  uploads;
- prompt source mounts should be read-only to the agent by default;
- if the agent can write prompt source paths, that is self-modifying prompt
  behavior and must be explicit;
- report whether each source path is writable through the current mount table;
- enforce UTF-8 and size limits before publishing;
- record missing optional files and ignored files in the report;
- fail clearly for required prompt sources once manifests exist.

## Provider Compatibility

OpenAI Responses is the first target:

- instruction context entries already materialize as top-level provider
  `instructions`;
- skill context entries already materialize as developer messages.

Anthropic Messages and OpenAI Chat Completions adapters are placeholders or
incomplete. P65 should not block on them, but prompt status should make clear
which provider paths actually receive instruction entries.

## Implementation Slices

### G1: Prompt Assembler

- Add `tools::prompts` model and assembler types.
- Support already-resolved text sources.
- Deterministically sort sources.
- Normalize line endings to `\n`.
- Preserve exact source CAS blobs for published instruction entries.
- Enforce per-source and total character limits by excluding oversize sources.
- Produce a report with hashes, sizes, warnings, publication status, and
  context keys.
- Unit test ordering, exact refs, limits, empty sources, and report stability.

### G2: VFS Prompt Roots

- Discover `.lightspeed/prompts` and `.agents/prompts` under workspace mounts.
- Resolve mounted snapshot/workspace roots.
- Read `instructions.md` and `instructions.d/*.md`.
- Record workspace id, revision, snapshot ref, path, content ref, and size.
- Test snapshot and workspace-backed prompt files.

### G3: Context Publication

- Add `prompt_source_instructions_context_input`.
- Add `prepare_prompt_instructions_publication`.
- Add `CoreAgentCommand::ReplaceContextPrefix`.
- Publish managed entries under `instructions.100.prompts`.
- No-op when the active managed prompt set is unchanged.
- Clear managed prompt entries when no prompt files remain.
- Test engine admission and planned context ordering.

### G4: Gateway Pre-Run Refresh

- Add idle pre-run refresh helper in `temporal-server`.
- Refresh prompts and skill catalog before `run/start` when idle.
- Leave active/queued sessions unchanged in G1.
- Add gateway tests for changed prompt files affecting the next run.

### G5: Prompts Active API

- Add read-only `prompts/active`.
- Return active prompt instruction entries, optional shared report ref, and
  decoded report JSON.
- Do not refresh prompt files or mutate session state from this endpoint.
- Avoid embedding full prompt file text in prompt or session reads.

### G6: Manual Refresh Command

- Add an explicit API/CLI refresh command for operators.
- Reuse the same idle policy as pre-run refresh.
- Return whether refresh was no-op, applied, removed, or rejected.

### G7: Automatic Reload

- Add a controller or workflow activity path that reacts to VFS workspace
  revision changes.
- Materialize prompt updates outside workflow replay.
- Store pending prompt updates while active work exists.
- Apply pending updates when the session becomes idle.

### G8: Prompt Overlays

- Add run-scoped overlays only after steering/context semantics support the
  desired role.
- Keep heartbeat and bootstrap as run-specific inputs or overlays, not
  permanent session instructions by default.
- Keep memory corpus retrieval separate from prompt instructions.

## Verification

Unit tests:

- deterministic source ordering,
- exact source CAS refs for published entries,
- UTF-8 validation,
- missing optional sources,
- size limits and unpublished-source warnings,
- report hash stability,
- no-op publication when content is unchanged.

Gateway tests:

- idle `run/start` refreshes prompt instructions before admitting the run,
- active or queued `run/start` does not refresh prompts in G1,
- removed prompt files clear stale managed prompt instruction entries,
- skill catalog still refreshes before idle `run/start`,
- prompt refresh failures return clear API errors before run admission.

Provider tests:

- OpenAI Responses receives active prompt source entries in top-level
  `instructions`,
- instruction context entries remain before other context entries,
- skill catalog and activation entries continue to render as developer
  messages.

## Open Questions

- Should `instructions.100.prompts` be the final prefix, or should product and
  project prompts get separate ordered keys?
- Should G1 include `instructions.d/*.md`, or only a single
  `instructions.md`?
- Should prompt reports be stored only in CAS, or also in workflow/session
  metadata for fast status reads?
- Should a missing prompt root remove active prompt entries immediately, or
  should removal require an explicit clear?
- What is the default total character budget for prompt instructions?

## Success Criteria

P65 is successful when:

- editing VFS prompt files changes the instructions used by the next idle
  `run/start`,
- the engine remains unchanged and deterministic,
- prompt provenance is visible through a report,
- active runs are not disrupted by prompt edits,
- skill catalog refresh and prompt refresh share the same pre-run freshness
  boundary,
- the design leaves room for later developer-message prompts, heartbeat,
  bootstrap, memory, and automatic reload.
