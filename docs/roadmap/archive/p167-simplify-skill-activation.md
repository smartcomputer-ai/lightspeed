# P167 — Simplify Skill Activation

Status: implemented, 2026-09-09.

The [VFS expansion freeze](p166-vfs-environment-transfer.md#vfs-expansion-freeze)
does not block removing the activation lifecycle. The existing workspace model
and VFS discovery remain supported; VFS retirement and the artifact replacement
are parked. Environment discovery publishes a separate catalog as described in
[environment skill catalogs](p168-environment-skill-catalogs.md), with no catalog
merging or dependency on automatic workspace materialization.

Make skill support a catalog plus ordinary file access. Remove the dedicated
activation lifecycle from the engine, API, and clients. The model finds a skill,
reads its `SKILL.md`, and follows its instructions using its existing tools.

Trust ordinary compaction to summarize ongoing work, including relevant skills.
Do not replace activation with protected file reads, a remembered-skill set,
automatic re-injection, or a new compaction policy.

## Previous behavior

The VFS skill catalog already supplies names, descriptions, and paths and asks
the model to read relevant skill files. Ordinary skill reads do not need an
activation operation.

Before this change, the separate explicit path included:

- `ContextEntryKind::SkillActivation` and reserved activation context keys.
- Run/session activation scopes and run-end expiry.
- Special compaction retention and provider rendering.
- Catalog-pinned skill documents used for explicit injection.
- API active/activate/deactivate methods, list-item active flags, and client
  commands and projections representing that state.

This duplicates the useful file-reading path and makes an ordinary instruction
document a special category of durable session state.

## Desired behavior

The model receives the current catalog and a brief instruction to read the
listed `SKILL.md` when relevant. The entry identifies the filesystem domain
and skill directory so the model can resolve relative references correctly.

```text
catalog entry -> ordinary file read -> ordinary tool result -> model follows it
```

There is no transition to an active skill state. Reading a reference document
or executing a bundled script remains an ordinary operation, with the normal
filesystem and tool permissions.

The catalog remains a current, retained context entry with existing
supersession and prompt-cache behavior. This change removes skill activation,
not the catalog machinery. Source-specific discovery and richer catalog
refresh are covered by [environment skill catalogs](p168-environment-skill-catalogs.md).

## Re-reads and mutable sources

A new read uses the ordinary semantics of its source:

- A live VFS workspace read resolves the current workspace contents.
- An environment read returns the current file on that environment.
- An explicitly linked immutable VFS snapshot remains immutable.

Do not pin skill bodies to the version observed during discovery or require
reactivation after an edit. A catalog is a menu, not a promise that a future
read will return the bytes seen by the catalog builder. A missing or changed
file is handled through ordinary reads and later catalog refresh.

Past tool results and explicitly inserted content still retain the bytes
originally recorded. Replaying history must not reopen a mutable source file.
This is ordinary conversation durability, not skill version management.

## Compaction

Skill file reads and explicitly inserted skill text use the same retention
rules as other conversation content. They may be compacted or removed from
active context. The current catalog retains its existing catalog treatment.

Rely on the existing compactor to carry useful task context forward. If the
model needs instructions again, it can read the skill again. Accept that
compaction may omit details; exact persistent skill-body retention is no longer
a guarantee of this feature.

Do not add:

- Skill-specific changes to compaction prompts or summaries.
- Protected tool results or file-content retention classes.
- A list of previously read or currently needed skills.
- Post-compaction hooks that re-read or re-inject skills.
- An activation flag under a different name.

Persistent mandatory guidance continues to use the existing instruction
mechanism. That is a separate authoring choice from selecting a skill for a task.

## Explicit user selection

Keep listing and selecting a skill convenient without retaining activation
state. A client selection can add an ordinary instruction such as:

```text
Use the release-review skill. Read it with vfs_read_file at
/library/release-review/SKILL.md before reviewing the release notes.
```

This is the preferred minimal replacement for CLI/TUI skill activation. It
uses ordinary run input or steering admission and does not require a special
idle-only activation mutation.

A client may instead read the selected file and insert its current contents
through the existing ordinary content-input path. Include its source and base
directory so supporting resources remain addressable. Such insertion has no
run/session scope, expiry, protection, or active flag. It is not a prerequisite
for completing the simpler read-instruction path.

Remove active-skill views and deactivation controls rather than retaining
compatibility commands with changed semantics. The UI may display that a file
was read in the transcript, as it does for any file, without projecting a new
skill lifecycle.

## Implementation boundaries

Use the generic `Catalog { title }` context kind for skill menus. The engine
need not interpret individual skill identities, paths, or instructions.
Move any remaining catalog-only identifiers out of engine ownership if removing
activation leaves them with no deterministic branching role.

Delete activation variants, validators, scope markers, expiry logic, dedicated
provider render branches, API DTOs/methods, and client state. Remove catalog
body references and containment edges that exist solely for explicit activation.
Retain source snapshot references required by genuine snapshot-backed catalogs
and the ordinary storage references required by recorded reads.

Do not remove unrelated media preprocessing or context append behavior merely
because those implementations also use the word activation. This proposal is
specifically about skill lifecycle state.

The existing parser and environment discovery improvements belong in
[the catalog proposal](p168-environment-skill-catalogs.md), and transferring a
directory for execution belongs in [VFS–environment transfer](p166-vfs-environment-transfer.md).
Neither feature is needed to remove VFS skill activation.

## Compatibility and rollout

This greenfield change removes public methods and persisted context variants
without backward compatibility. Update generated contracts and all consumers
together. Existing histories containing activation entries are not supported
by the new engine. Deployment must use fresh or separately retired development
state; the implementation does not reset databases or workflows.

Keep archived design documents historical. Update the current workspace/skills
guide, API reference, CLI help, demos, and README when implementation ships.

## Verification and completion

1. Remove the engine lifecycle and its provider/API/client surface together.
2. Preserve VFS catalog discovery, stable unchanged catalogs, and supersession.
3. Verify skill reads are ordinary results and a later read sees a workspace
   edit, while replay retains the earlier recorded result.
4. Verify explicit selection creates ordinary input with the correct source
   and path. Existing instruction and media behavior remains covered.
5. Update compaction tests to assert ordinary eligibility of skill reads and
   inserted text, not exact survival or automatic recovery of skill instructions.
6. Regenerate API and affected workflow contracts, then run scoped checks for
   engine, tools, provider adapters, gateway/workflow, CLI, and TypeScript clients.

No new credentialed compaction experiment is required by this design. Any
future evidence that ordinary compaction is insufficient is a separate issue,
not a reason to retain skill activation machinery here.

Progress:

- [x] Engine and provider activation handling removed.
- [x] API/client activation state removed; ordinary selection retained.
- [x] Catalog and ordinary-read behavior verified.
- [x] Generated contracts and current documentation updated.

Implemented client selection uses `skills use --session <session-id> <skill-id>`
and the TUI `/skill` picker or `/skill <skill-id>`. Both submit ordinary input
or steering with the VFS document path and base directory from
`session/skills/list`. There is no replacement activation API.

Verification completed across engine, tools, provider adapters, API and
projection, gateway, workflow, CLI, and the test runner: 1,282 Rust tests passed
after the targeted expectation corrections; 121 credentialed/live tests stayed
ignored. All 362 TypeScript consumer tests, typechecks, web/demo builds, and docs
checks passed. API and workflow exporters ran; the workflow export was unchanged.
TypeScript and Configurator outputs were regenerated and a second generation
was byte-identical. The npm generated-file gate reports the intentional
uncommitted diff against HEAD; its remaining checks passed separately.


## Shared catalog representation

Follow-up simplification before environment skill discovery:

- [x] Replace specialized VFS, skill, and sub-agent context kinds with `Catalog { title }`.
- [x] Render text at publication and retain each structured source in provenance.
- [x] Share publication comparison, clearing, and gateway acknowledgement handling.
- [x] Carry current keyed catalog inputs through workflow refresh instead of per-domain references.
- [x] Reserve `runtime.*` context keys at the public API boundary; keep engine handling generic.
- [x] Verify independent supersession, replay, source retention, provider rendering, and regenerated consumers.

The VFS route, skill metadata, and sub-agent snapshots remain distinct source
models. Environment discovery, within-run refresh, catalog merging, and
workspace materialization remain deferred.

Verification: 1,287 Rust tests passed across engine, tools, provider adapters,
API/projection, gateway/workflow, CLI, and the test runner; 121 live tests stayed
ignored. The final publisher text/provenance assertion also passed in a focused
run. All 362 TypeScript consumer tests, typechecks, web/demo builds, and docs
checks passed. API and workflow exporters ran; the workflow export was
unchanged. Repeating consumer generation was byte-identical. The npm
generated-file gate reports the intentional uncommitted diff against HEAD;
its remaining checks passed separately.
