# P71: Prompt Management And VFS Prompt Bundles

**Status**
- Proposed
- Builds on P62 CAS/VFS and the current `SessionConfig.context.instructions_ref`
  path.
- Intended to give Forge an editable prompt surface similar in spirit to
  OpenClaw, without moving file I/O, watches, or prompt assembly into the
  deterministic engine.

## Goal

Add a first-class prompt management layer that lets Forge agents assemble their
effective session instructions from explicit, inspectable sources such as:

- inline text,
- CAS blobs,
- VFS files,
- VFS workspace snapshots,
- product/runtime prompt contributions,
- future memory, skill, heartbeat, and bootstrap contributions.

The result of prompt assembly should still be a normal CAS blob referenced by
`SessionConfig.context.instructions_ref`. The engine should continue to see the
compiled instructions as an immutable blob ref, not as live files or a mutable
prompt graph.

This gives Forge:

- a practical prompt editing surface,
- deterministic prompt materialization,
- visible prompt provenance and size reports,
- safe reload behavior for long-running sessions,
- a path to VFS-backed prompt files without compromising engine determinism.

## Background

Forge already has a low-level prompt injection mechanism. It is deliberately
small:

- `api` accepts `SessionConfigInput.instructions` as inline text or a CAS blob
  ref in `crates/api/src/lib.rs`.
- `temporal-server` converts that API value into a `BlobRef`, writing inline
  instructions to CAS or validating an existing blob ref in
  `crates/temporal-server/src/gateway/service.rs`.
- `temporal-workflow` writes default instructions to CAS when a session is
  opened without an explicit instructions ref in
  `crates/temporal-workflow/src/workflow.rs`.
- `engine` stores only `ContextConfig.instructions_ref` in
  `crates/engine/src/core/components/config.rs`.
- `engine` copies that ref into provider-native LLM request structs in
  `crates/engine/src/core/components/llm.rs`.
- `llm-runtime` reads the blob and materializes it into the provider request.
  For OpenAI Responses, it becomes the provider top-level `instructions` field
  in `crates/llm-runtime/src/openai_responses.rs`.
- `api-projection` reads the blob back into `SessionConfigView.instructions` for
  clients in `crates/api-projection/src/lib.rs`.

That means current Forge instructions are not inserted as the first user or
assistant message. They are separate provider instructions/system state where
the provider supports that shape.

The current path is intentionally primitive: one effective blob. This is a good
narrow waist for the engine and provider adapters, but it is not enough for an
operator-friendly prompt editing surface.

## Current Forge References

Runtime model:

- `README.md` documents that `session/start` accepts model, instructions,
  generation, context, and run defaults, and that instructions can be inline
  text or a CAS blob ref.
- `spec/01-agent-idea.md` states that an agent consists of initial conditions,
  prompts, model config, tools, context management, external inputs over time,
  and event-driven configuration changes.

Low-level API and config:

- `crates/api/src/lib.rs`
  - `SessionConfigInput.instructions`
  - `InstructionsSource::{Text, BlobRef}`
  - `SessionConfigPatchInput.instructions`
- `crates/temporal-server/src/gateway/service.rs`
  - `session_config_for_start`
  - `apply_session_config_input`
  - `instructions_ref_from_source`
  - `core_session_patch_from_api`
- `crates/temporal-workflow/src/workflow.rs`
  - `open_new_session` default-instructions CAS write
- `crates/temporal-workflow/src/config.rs`
  - `default_session_config`
  - `default_instructions`

Engine and runtime:

- `crates/engine/src/core/components/config.rs`
  - `ContextConfig.instructions_ref`
  - `ContextConfigPatch.instructions_ref`
  - config updates require the session to be idle
- `crates/engine/src/core/components/llm.rs`
  - `OpenAiResponsesRequest.instructions_ref`
  - `AnthropicMessagesRequest.system_ref`
  - `OpenAiCompletionsRequest` currently has no instructions/system ref
- `crates/llm-runtime/src/openai_responses.rs`
  - provider request materialization reads `instructions_ref`
- `crates/api-projection/src/lib.rs`
  - projects `instructions.blob_ref` and text

VFS:

- `crates/vfs/src/lib.rs` explicitly keeps host filesystem access,
  materialization, and process execution outside the VFS crate.
- `crates/vfs/src/snapshot.rs` can create/read immutable CAS-backed snapshot
  manifests and read files from them.
- `crates/vfs/src/catalog.rs` defines mutable VFS workspaces with
  `head_snapshot_ref` and `revision`.
- `crates/store-fs/src/vfs.rs` and `crates/store-pg/src/vfs.rs` provide first
  store implementations.

## OpenClaw Reference

This design is inspired by the OpenClaw prompt surface, not a direct port.

Reference study paths from the local OpenClaw checkout:

- `/Users/lukas/dev/tmp/openclaw/src/agents/system-prompt.ts`
  - defines a default ordering for context files such as `AGENTS.md`,
    `SOUL.md`, `IDENTITY.md`, `USER.md`, `TOOLS.md`, `BOOTSTRAP.md`, and
    `MEMORY.md`;
  - treats `HEARTBEAT.md` as dynamic context;
  - renders loaded files into system prompt sections;
  - separates stable and dynamic prompt material to help provider prompt cache
    behavior.
- `/Users/lukas/dev/tmp/openclaw/src/agents/bootstrap-files.ts`
  - resolves prompt/context files for a run;
  - applies filtering for heartbeat, bootstrap, and context injection modes;
  - builds prompt-ready context files with size budgets.
- `/Users/lukas/dev/tmp/openclaw/src/agents/workspace.ts`
  - owns the default workspace file names and creation path for OpenClaw's
    prompt editing surface.
- `/Users/lukas/dev/tmp/openclaw/src/plugins/memory-state.ts`
  - keeps memory prompt guidance and memory runtime/search capability as
    separate plugin-owned concepts.
- `/Users/lukas/dev/tmp/openclaw/src/agents/system-prompt-report.ts`
  - builds observability for prompt size, hashes, injected files, skills, and
    tool schemas.
- `/Users/lukas/dev/tmp/openclaw/src/agents/embedded-agent-runner/run/attempt-system-prompt.ts`
  - builds the per-attempt prompt and applies provider prompt transforms.

Things worth copying:

- editable markdown files as the operator surface,
- deterministic ordering,
- stable versus dynamic prompt sections,
- prompt source reports with hashes and sizes,
- special treatment for memory and heartbeat rather than dumping everything
  into one permanent prompt,
- re-rendering the system prompt per attempt/session boundary from current
  source state.

Things not to copy directly:

- magic filenames hardcoded into Forge engine semantics,
- host filesystem reads inside the deterministic loop,
- per-turn live file polling in engine,
- provider/runtime-specific prompt cache hacks as the first abstraction,
- treating memory as one unbounded markdown prompt file.

## Problem Statement

The current `instructions` field is an escape hatch, not a prompt management
system.

It cannot answer operational questions like:

- Which files contributed to the current prompt?
- Which exact VFS revision was used?
- Did a prompt file change after the current session started?
- Will a prompt edit affect the current run or only the next one?
- Which parts are stable persona versus heartbeat/bootstrap/run-specific
  steering?
- What was truncated?
- Can the agent edit the files that define its own instructions?
- How does a UI expose prompt editing without asking users to paste a giant
  instructions blob?

Forge needs prompt management, but the engine should not grow a prompt
filesystem, watch service, or mutable prompt source resolver.

## Non-Goals

- Do not put file watches, VFS reads, host filesystem reads, or prompt assembly
  in `engine`.
- Do not replace `ContextConfig.instructions_ref` as the low-level effective
  instructions mechanism in the first implementation.
- Do not require standardized prompt filenames such as `SOUL.md` or
  `MEMORY.md`.
- Do not make prompt reload mutate old context items, old provider requests, or
  already-completed assistant behavior.
- Do not build a full memory system in this phase.
- Do not make all prompt sources model-visible files; inline and blob-backed
  sources remain useful.
- Do not make OpenAI Chat Completions parity block the first implementation,
  but record the current gap.
- Do not treat untrusted VFS trees as safe system prompt material without an
  explicit trust boundary.

## Design Position

Prompt management is a runtime/API concern that compiles to the existing engine
narrow waist.

Target shape:

```text
prompt config / agent profile / UI edits
  -> gateway or worker prompt resolver
  -> CAS/VFS reads outside engine
  -> pure prompt assembler
  -> compiled instructions blob in CAS
  -> SessionConfig.context.instructions_ref
  -> existing engine and LLM runtime path
```

The deterministic engine should only record and replay:

- effective instructions blob refs,
- config revisions,
- run inputs,
- context items,
- future prompt report refs if needed.

It should not know whether an instructions blob came from one pasted string,
five VFS files, a generated runtime contribution, or an OpenClaw-like prompt
workspace.

## Core Concepts

### Effective Instructions

The effective instructions are the single text blob used by provider request
builders as system/instructions content.

In the first implementation, prompt management always compiles to:

```rust
SessionConfig {
    context: ContextConfig {
        instructions_ref: Some(compiled_prompt_ref),
        ..
    },
    ..
}
```

This keeps provider request planning and execution mostly unchanged.

### Prompt Bundle

A prompt bundle is an ordered set of prompt sources plus assembly and reload
policy.

It is product/API-level configuration. It should not be a core engine state
machine by itself.

Example JSON shape:

```json
{
  "prompt": {
    "sources": [
      {
        "id": "base",
        "type": "text",
        "order": 10,
        "text": "You are Forge."
      },
      {
        "id": "persona",
        "type": "vfsFile",
        "order": 20,
        "workspaceId": "agent-main",
        "path": "/prompts/SOUL.md",
        "required": false,
        "maxChars": 12000
      },
      {
        "id": "operator-memory",
        "type": "vfsFile",
        "order": 30,
        "workspaceId": "agent-main",
        "path": "/prompts/MEMORY.md",
        "required": false,
        "maxChars": 12000
      }
    ],
    "reload": {
      "mode": "onWorkspaceRevision",
      "apply": "whenIdle"
    }
  }
}
```

The file names above are examples. Forge should support this style without
hardcoding those names into the engine.

### Prompt Source

First-cut source kinds:

```rust
pub enum PromptSourceInput {
    Text {
        id: PromptSourceId,
        order: i32,
        text: String,
    },
    BlobRef {
        id: PromptSourceId,
        order: i32,
        blob_ref: String,
    },
    VfsFile {
        id: PromptSourceId,
        order: i32,
        source: VfsPromptFileSource,
    },
}

pub struct VfsPromptFileSource {
    pub workspace_id: VfsWorkspaceId,
    pub path: VfsPath,
    pub required: bool,
    pub max_chars: Option<u32>,
    pub media_type: Option<String>,
}
```

Potential later source kinds:

- `VfsGlob`
- `SkillPromptContribution`
- `RuntimePromptContribution`
- `MemoryPromptGuidance`
- `PromptBundleRef`
- `AgentProfileRef`

Avoid adding glob support in G1 unless a product surface needs it. Explicit
paths are easier to make safe, deterministic, and explainable.

### Prompt Source Purpose

Not every prompt-like thing should become permanent session instructions.

Use these purpose classes conceptually, even if the first implementation only
compiles `SessionInstructions`:

```rust
pub enum PromptSourcePurpose {
    SessionInstructions,
    RunSteering,
    PromptReportOnly,
}
```

Examples:

- persona, general tool-use policy, and operator preferences are session
  instructions;
- heartbeat and bootstrap are often run-scoped steering;
- large memory corpora are reportable/searchable sources, not permanent system
  text.

The first implementation can reject non-session purposes until run steering is
complete enough to carry them.

### Prompt Materialization

Prompt materialization resolves a bundle into one compiled prompt blob and a
report.

Inputs:

- bundle config,
- optional current session id/config revision,
- current VFS workspace head records,
- blob store,
- VFS catalog/store,
- size limits.

Outputs:

```rust
pub struct PromptMaterialization {
    pub instructions_ref: BlobRef,
    pub report_ref: BlobRef,
    pub report: PromptMaterializationReport,
}

pub struct PromptMaterializationReport {
    pub schema_version: String,
    pub generated_at_ms: i64,
    pub instructions_ref: BlobRef,
    pub instructions_sha256: String,
    pub total_chars: u32,
    pub total_bytes: u64,
    pub sources: Vec<PromptSourceReport>,
    pub warnings: Vec<PromptWarning>,
}

pub struct PromptSourceReport {
    pub id: PromptSourceId,
    pub order: i32,
    pub kind: String,
    pub status: PromptSourceStatus,
    pub chars: u32,
    pub bytes: u64,
    pub sha256: String,
    pub blob_ref: Option<BlobRef>,
    pub vfs_workspace_id: Option<VfsWorkspaceId>,
    pub vfs_workspace_revision: Option<u64>,
    pub vfs_snapshot_ref: Option<BlobRef>,
    pub vfs_path: Option<VfsPath>,
    pub truncated: bool,
}
```

The report should be stored in CAS. The session view can expose either the full
report or a report ref depending on API size.

### Source Wrapping

The assembler should wrap file sources with stable section markers so reports
and humans can correlate prompt text to sources.

Example rendered shape:

```markdown
# Forge Instructions

## base

You are Forge.

## persona

Source: vfs://agent-main@42/prompts/SOUL.md

...
```

The exact wrapper should be boring and predictable. It should avoid giving
model-visible authority to paths themselves. A source path is provenance, not a
priority rule.

### Stable And Dynamic Sections

OpenClaw separates stable and dynamic prompt material so provider prompt caches
can reuse the stable prefix.

Forge should model this as metadata, not as an OpenClaw-specific cache hack:

```rust
pub enum PromptCacheGroup {
    Stable,
    Dynamic,
}
```

Default:

- most session instructions are `Stable`,
- heartbeat and current-runtime facts are `Dynamic`,
- frequently edited operator notes can opt into `Dynamic`.

For G1 this may only affect ordering/reporting. Provider-specific prompt cache
behavior can come later.

## Assembly Rules

Prompt assembly must be deterministic for the same resolved inputs.

Rules:

- Sort sources by `order`, then `id`.
- Normalize line endings to `\n`.
- Trim only according to explicit source policy. Do not silently drop content
  except empty optional sources.
- Validate UTF-8 for text sources.
- Reject non-text VFS files unless explicitly configured with a text media type
  or decoder.
- Enforce per-source and total prompt limits.
- Record every missing optional source in the report.
- Fail materialization when a required source is missing.
- Record truncation in both the report and model-visible text if truncation
  changes source content.
- Store the final assembled text in CAS before opening or patching a session.

Open question: total prompt limit should probably be character-based first and
token-aware later. Token-aware limits require provider/model tokenizers and
should not block G1.

## VFS Resolution

VFS prompt sources should resolve through workspace revisions, not live paths.

Resolution flow:

```text
PromptSource::VfsFile(workspace_id, path)
  -> VfsWorkspaceStore::read_workspace(workspace_id)
  -> head_snapshot_ref + revision
  -> read_snapshot_manifest(head_snapshot_ref)
  -> read_snapshot_file(path)
  -> source report records workspace_id, revision, snapshot_ref, path, file blob ref/hash
```

This matches the existing VFS design:

- snapshots are immutable CAS manifest blobs,
- workspaces are mutable heads with revisions,
- compare-and-set protects workspace updates,
- engine sees only blob refs.

Do not use OS file watchers as the durable source of truth. A host watcher may
exist, but its job is to create a new VFS snapshot/workspace revision. Prompt
reload reacts to VFS revision changes.

## Reload Semantics

Prompt reload should be explicit and session-safe.

Recommended first-cut policy:

```rust
pub struct PromptReloadPolicy {
    pub mode: PromptReloadMode,
    pub apply: PromptReloadApplyPolicy,
}

pub enum PromptReloadMode {
    Manual,
    OnWorkspaceRevision,
}

pub enum PromptReloadApplyPolicy {
    WhenIdle,
    RejectIfActive,
}
```

Default:

- `mode = Manual` for low-level API users,
- `mode = OnWorkspaceRevision` for product-managed prompt workspaces,
- `apply = WhenIdle`.

When a source VFS workspace revision changes:

1. Resolve and materialize a new prompt bundle.
2. Compare the new compiled prompt hash to the last applied hash.
3. If unchanged, record/report no-op.
4. If changed and the session is idle, send `session/update` with a patch that
   sets `instructions_ref` to the new compiled blob.
5. If the session has an active or queued run, store a pending prompt update and
   apply it when the session reaches idle.

This matches the current engine rule that config updates can only happen while
no run is active or queued.

### Active Run Behavior

An active run continues with the prompt that was current when its request was
planned. Prompt reload never rewrites an in-flight provider request.

If the prompt changes mid-run:

- the current run is unaffected,
- the pending prompt is visible in prompt status,
- the next run uses the new prompt after the config update is applied.

### Existing History Behavior

Prompt updates do not rewrite session history.

Old messages remain old messages. New instructions affect future LLM requests
only. If a prompt edit is semantically incompatible with prior context, the
operator should start a new session, fork later if supported, or trigger
compaction with an explicit summary policy.

This avoids a dangerous illusion that a session's old assistant behavior was
produced under instructions that did not exist yet.

## Session Start Flow

First-cut `session/start` with a prompt bundle:

```text
client -> gateway: session/start(config.prompt = bundle)
gateway:
  resolve bundle sources
  materialize prompt to CAS
  set SessionConfig.context.instructions_ref
  optionally store prompt report ref in session metadata/projection store
gateway -> workflow: AgentSessionArgs { session_config }
workflow -> engine: CoreAgentCommand::OpenSession { config }
```

The existing `config.instructions` path should remain as the low-level escape
hatch. If both `instructions` and `prompt` are supplied, prefer one of these
rules:

- reject the request as ambiguous, or
- treat `instructions` as an additional prompt source only if explicitly
  wrapped by `prompt.sources`.

Recommendation: reject both in G1. It keeps the API honest.

## Session Update Flow

First-cut `session/update` with prompt changes:

```text
client -> gateway: session/update(patch.prompt = new bundle)
gateway:
  read current session/config
  resolve and materialize bundle
  build CoreAgentCommand::PatchSessionConfig {
    patch.context.instructions_ref = Set(compiled_prompt_ref)
  }
workflow/engine:
  accept only while idle
```

If automatic reload is enabled, the prompt manager performs the same operation
when watched source revisions change.

## Prompt Status And Observability

Forge should expose prompt status before deep UI work starts.

Candidate API:

```text
session/prompt/read
```

or, after P60:

```text
query/read { type: "forge.session.prompt.read", session_id }
```

Candidate response:

```json
{
  "sessionId": "session_1",
  "applied": {
    "instructionsRef": "sha256:...",
    "reportRef": "sha256:...",
    "hash": "sha256:...",
    "totalChars": 18420,
    "generatedAtMs": 123
  },
  "pending": null,
  "sources": [
    {
      "id": "persona",
      "kind": "vfsFile",
      "workspaceId": "agent-main",
      "workspaceRevision": 42,
      "snapshotRef": "sha256:...",
      "path": "/prompts/SOUL.md",
      "chars": 2400,
      "hash": "sha256:...",
      "truncated": false
    }
  ],
  "warnings": []
}
```

This is the Forge equivalent of OpenClaw's prompt breakdown/report, but backed
by CAS/VFS refs instead of process-local files.

## Prompt Editing Surface

The editing surface should be a normal VFS workspace.

Example workspace:

```text
/prompts/
  persona.md
  operating-style.md
  memory-guidance.md
  heartbeat.md
```

A product profile can map those files into a bundle:

```json
{
  "sources": [
    {
      "id": "persona",
      "type": "vfsFile",
      "order": 20,
      "workspaceId": "agent-main",
      "path": "/prompts/persona.md"
    },
    {
      "id": "memory-guidance",
      "type": "vfsFile",
      "order": 30,
      "workspaceId": "agent-main",
      "path": "/prompts/memory-guidance.md"
    }
  ]
}
```

Forge can offer OpenClaw-like templates, but templates should be conventions:

- `SOUL.md` can mean persona/tone if a product profile chooses that mapping.
- `MEMORY.md` can mean durable preference guidance if mapped.
- `HEARTBEAT.md` can mean heartbeat instructions if the heartbeat run kind maps
  it.

The engine should not assign special meaning to those names.

## Bootstrap, Heartbeat, And Memory

### Bootstrap

Bootstrap is not just a static prompt file. It often means "one-time setup or
orientation work until completed."

First-cut option:

- allow a bootstrap prompt source in session instructions,
- include a report field that marks it as bootstrap,
- leave completion semantics outside P71.

Better later option:

- model bootstrap as run-scoped steering or a special first-run input,
- record completion through a durable event,
- stop injecting bootstrap once completion is observed.

### Heartbeat

Heartbeat should be run-scoped by default.

OpenClaw treats `HEARTBEAT.md` as dynamic context and filters it depending on
run kind. Forge should follow that conceptually:

- heartbeat prompt files can be VFS sources,
- heartbeat files should not become permanent instructions for all runs unless
  the operator explicitly configures them that way,
- a heartbeat run should include heartbeat source content as run steering or as
  a heartbeat-specific prompt bundle overlay.

This likely needs a completed steering-context path. Current engine state has
`RequestRunSteering` and `ActiveRun.steering_refs`, but steering refs are not
yet planned into the context window. That gap should be closed before relying
on heartbeat as steering.

### Memory

Memory should be split:

- prompt guidance for how to use memory,
- retrieval/search tools for actual memory facts,
- selected retrieved memories as bounded context items,
- memory write/flush policy outside the system prompt.

Do not paste an unbounded memory corpus into `instructions_ref`. A small
`memory-guidance.md` prompt source is fine. Long-term memory content should be
retrieved and cited through a memory subsystem.

## Provider Compatibility

OpenAI Responses:

- already has `OpenAiResponsesRequest.instructions_ref`;
- `llm-runtime` materializes it as provider `instructions`;
- good first target.

Anthropic Messages:

- engine request shape already has `AnthropicMessagesRequest.system_ref`;
- runtime adapter work may still be needed depending on current provider
  implementation stage.

OpenAI Chat Completions:

- current `OpenAiCompletionsRequest` has no `instructions_ref` or `system_ref`;
- prompt management can still compile instructions, but that provider path will
  not receive them until the request shape and adapter are extended.

Recommendation:

- implement P71 against OpenAI Responses first;
- add provider tests that fail clearly for unsupported provider API kinds or
  extend Completions before exposing prompt bundles there.

## Trust And Safety

Prompt source files are trusted operator state.

If an agent can write to the same VFS paths that define its own system
instructions, that is self-modifying prompt behavior. It may be useful, but it
must be explicit.

Rules:

- prompt workspaces should default to read-only mounts for the agent;
- write access to prompt source paths should require explicit user action or
  a narrowly scoped tool policy;
- prompt status should identify writable prompt sources;
- untrusted uploaded files should never become system prompt sources by
  default;
- VFS path normalization and size limits must be enforced before reading;
- missing optional prompt sources should be visible, not silent;
- required missing sources should fail materialization.

Prompt injection remains possible inside trusted prompt files. That is not a
security boundary bypass by itself. The security boundary is who can write those
trusted prompt files and under what approval policy.

## Crate And Module Placement

Recommended first layout:

```text
crates/prompt
  pure prompt source/report/assembly types
  no host filesystem I/O
  no VFS catalog implementation
  no Temporal dependency

crates/api
  PromptBundleInput API types
  prompt status/query response types

crates/temporal-server
  API-to-materialization orchestration for local/hosted paths
  VFS/blob source resolution
  session/start and session/update prompt compilation

crates/temporal-workflow / crates/temporal-server
  optional prompt materialization activities for hosted reload paths
  history records refs/revisions/hashes, not raw file reads

crates/api-projection
  expose prompt report summaries and instructions refs in SessionConfigView
```

If a separate crate feels too early, put pure assembly code in a gateway module
first, but keep it side-effect-free and easy to extract.

Do not put the assembler in `engine`.

## Temporal Position

Temporal workflows must stay deterministic.

For hosted automatic reload:

```text
workflow receives signal: prompt source workspace changed or prompt refresh requested
workflow starts activity: materialize_prompt_bundle
activity:
  reads VFS catalog + blobs
  writes compiled prompt + report to CAS
  returns refs, revisions, hashes
workflow:
  records result in workflow state
  admits PatchSessionConfig when session is idle
```

The workflow history should contain:

- prompt bundle config or bundle ref,
- source workspace ids,
- source revisions,
- compiled prompt blob ref,
- report ref,
- hashes,
- applied/pending status.

It should not contain host filesystem reads or rely on process-local watcher
state for replay.

## API Sketch

Keep low-level `instructions` and add ergonomic `prompt`.

```rust
pub struct SessionConfigInput {
    pub instructions: Option<InstructionsSource>,
    pub prompt: Option<PromptBundleInput>,
    pub model: Option<ModelConfig>,
    pub generation: Option<GenerationConfig>,
    pub context: Option<ContextConfigInput>,
    pub run_defaults: Option<RunDefaultsConfig>,
}

pub struct SessionConfigPatchInput {
    pub instructions: Option<FieldPatch<InstructionsSource>>,
    pub prompt: Option<FieldPatch<PromptBundleInput>>,
    pub model: Option<ModelConfig>,
    pub generation: Option<GenerationConfigPatch>,
    pub context: Option<ContextConfigPatchInput>,
    pub run_defaults: Option<RunDefaultsPatch>,
}
```

Rules:

- reject `instructions` and `prompt` together in the same request or patch;
- `instructions` remains a raw low-level instructions source;
- `prompt` is materialized to instructions before entering engine;
- clear prompt should either clear instructions or revert to default
  instructions, but this must be explicit in API semantics.

Potential `SessionConfigView` extension:

```rust
pub struct SessionConfigView {
    pub model: ModelConfig,
    pub instructions: Option<InstructionsView>,
    pub prompt: Option<PromptBundleView>,
    pub generation: GenerationConfig,
    pub context: ContextConfigInput,
    pub run_defaults: RunDefaultsConfig,
}
```

The view should not require embedding the full compiled prompt text every time.
Large text can remain behind `instructions.blob_ref` and `prompt.report_ref`.

## Implementation Slices

### G1: Pure Prompt Assembly Model

- Add prompt source, resolved source, materialization report, warning, and
  assembler types.
- Support text and already-resolved blob/text sources.
- Deterministic order by `order`, then `id`.
- Add source wrappers, normalization, per-source limits, total limits, hashes,
  and truncation reporting.
- Unit tests cover ordering, missing optional sources, required source failure,
  truncation, hash stability, and report shape.

### G2: API Types And Validation

- Add `PromptBundleInput` to `api`.
- Add `PromptBundleView` or prompt report view types.
- Validate that `instructions` and `prompt` are not supplied together.
- Keep existing API backward-compatible for low-level instructions users.

### G3: Gateway Materialization For Session Start

- Resolve `PromptSourceInput::Text` and `PromptSourceInput::BlobRef`.
- Write compiled prompt and report to CAS.
- Set `SessionConfig.context.instructions_ref`.
- Expose the report through session read or a prompt read method.
- Add gateway tests proving `session/start` with a bundle creates the expected
  instructions blob.

### G4: VFS File Sources

- Resolve VFS workspace head revision and snapshot ref.
- Read VFS file bytes from the snapshot manifest.
- Validate UTF-8 and text media type.
- Record workspace id, revision, snapshot ref, path, file blob/hash, and size in
  the report.
- Add store-fs/store-pg-backed tests where possible.

### G5: Session Update And Manual Refresh

- Add prompt bundle update support.
- Materialize prompt patches before building `CoreAgentCommand::PatchSessionConfig`.
- Respect existing idle-only engine config update rule.
- Add tests for successful idle update and active-session rejection/pending
  behavior depending on selected policy.

### G6: Automatic Reload On VFS Revision

- Add a prompt manager/controller outside engine that tracks source workspace
  revisions for sessions with reload enabled.
- When revisions change, materialize a candidate prompt.
- If the compiled hash changed, apply when idle or store pending.
- Make pending/applied status visible.
- Do not depend on OS watches as the source of truth.

### G7: Prompt Editing Surface

- Define a default product prompt workspace convention.
- Provide CLI/API helpers to initialize prompt files in VFS.
- Provide read/update/status commands for the prompt bundle and prompt files.
- Keep all default names configurable.

### G8: Run-Scoped Prompt Overlays

- Complete steering/context support if needed.
- Add heartbeat/bootstrap overlays as run-scoped prompt sources rather than
  permanent instructions.
- Keep memory corpus retrieval separate from system prompt assembly.

## Verification

Unit tests:

- deterministic assembly ordering,
- source wrapper stability,
- UTF-8 validation,
- optional versus required missing sources,
- per-source and total truncation,
- source hash/report stability,
- no-op materialization when source hashes do not change.

Gateway/API tests:

- `session/start` with prompt bundle compiles to `instructions_ref`,
- `session/start` rejects both raw instructions and prompt bundle,
- `session/update` with prompt bundle patches `instructions_ref`,
- prompt report is projected/readable,
- active session prompt update follows the configured reject/pending behavior.

VFS tests:

- VFS file prompt source reads the exact workspace head revision,
- subsequent workspace revision materializes a different prompt,
- missing optional VFS source is reported,
- missing required VFS source fails.

Workflow tests:

- hosted prompt materialization uses activities for VFS/CAS reads,
- workflow history records refs/revisions/hashes,
- reload applies only when idle.

Provider tests:

- OpenAI Responses request contains compiled instructions text,
- unsupported provider API kinds fail clearly or are gated until supported,
- changing prompt changes future request fingerprints through the new
  `instructions_ref` but does not rewrite existing context.

## Open Questions

- Should prompt bundle config be stored in the engine config, session metadata,
  workflow state, or a product-level registry?
  - Recommendation: not in engine for G1. Store it in API/workflow/product state
    and compile to `instructions_ref`.
- Should prompt reports be first-class session records or CAS blobs referenced
  by projections?
  - Recommendation: CAS blob plus projected summary first.
- Should automatic reload be implemented by gateway, worker, workflow, or a
  separate controller?
  - Recommendation: gateway/manual first; worker/workflow activity path for
    hosted automatic reload once product behavior is clear.
- Should `PromptBundleInput` support VFS globs?
  - Recommendation: defer. Explicit files are safer and easier to explain.
- What should clearing a prompt bundle do?
  - Options: clear instructions entirely, restore default instructions, or
    restore previous raw instructions. Pick one explicit API behavior before
    G2.
- Should stable/dynamic cache grouping affect provider request construction in
  G1?
  - Recommendation: report-only in G1.
- Should prompt source files be mounted into the agent-visible workspace?
  - Recommendation: yes for editing, but read-only by default for the running
    agent.

## Success Criteria

P71 is successful when:

- a session can start from a prompt bundle instead of one raw instructions blob,
- VFS prompt files can be edited and rematerialized into new instructions,
- prompt reports show exact source provenance, revisions, hashes, and
  truncation,
- prompt changes apply to future runs without rewriting old history,
- active runs are not disrupted by prompt reload,
- the engine remains deterministic and only sees blob refs/config patches,
- OpenAI Responses receives the compiled prompt through the existing
  provider-native instructions path,
- the design leaves room for heartbeat, bootstrap, memory, and skills without
  turning them all into one permanent system prompt.
