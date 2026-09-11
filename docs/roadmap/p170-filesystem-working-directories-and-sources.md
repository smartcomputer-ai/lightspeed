# P170 — Working directories and prompt/skill sources

Status: implemented, 2026-09-11.

Give VFS and environments independent session working directories, and align
prompt loading and skill discovery across both domains. This builds on
[environment catalogs](p168-environment-skill-catalogs.md). It retains the current
VFS workspace model; the conflicting VFS-removal direction in
[immutable artifacts](p169-immutable-artifacts.md) is outside this proposal and
must not be applied to this implementation.

## Configuration

```json
{
  "features": {
    "vfs": {
      "workingDirectory": "/workspace",
      "prompts": {},
      "skills": {}
    },
    "environments": {
      "workingDirectory": "/workspace/project",
      "prompts": {},
      "skills": { "roots": ["./.agents/skills", "/opt/team-skills"] }
    }
  }
}
```

Workspace links and other grants are omitted from this example. The two
filesystem domains remain separate; no implicit copying or synchronization.

## Working directories

- `features.vfs.workingDirectory`: optional absolute VFS path. Defaults to `/`;
  remove the existing inference that selects `/workspace` when that link exists.
  Relative VFS tool paths resolve against this directory. Validate configured
  paths against the linked namespace; `/` remains its synthetic root.
- `features.environments.workingDirectory`: optional absolute machine path.
  Defaults to the selected environment's advertised working directory. File
  tools, processes, jobs, and discovery use this same base. Resolve and validate
  it on the selected machine; invalid paths fail clearly without silent fallback.
- A command may override `cwd`. Its `cd` does not persist into later commands,
  file operations, or discovery. Environment switching resolves the session's
  configured directory on the newly selected machine.

## Matching source capabilities

Both domains have independent optional `prompts` and `skills` blocks:

| Configuration | Behavior |
| --- | --- |
| Block absent | Disabled; remove that source's runtime projection. |
| `{}` | Enabled using conventional roots. |
| `{ "roots": ["…"] }` | Enabled using only those roots, replacing every default. |

Explicit lists must be nonempty. Roots identify the actual prompt/skill
directories; conventional suffixes are not appended to overrides.

| Domain | Default locations |
| --- | --- |
| VFS | Beneath each workspace link, including snapshot links. No links means no sources. |
| Environment | Beneath the effective working directory and execution user's home, deduplicated when equal. |

Only `.agents/prompts` and `.lightspeed/prompts`, or `.agents/skills` and
`.lightspeed/skills`, are searched automatically. Never infer `.claude` or
`.codex` roots. Users can explicitly list those directories if desired. There
is no ancestor search or separate home-directory switch; explicit roots also
replace home defaults.

VFS overrides are absolute paths inside workspace links. Environment overrides
may be absolute or relative to the effective working directory. Remove
`environments.skills.workingDirectory`, `projectRoot`, and `additionalRoots`.
The domain working directory and optional source `roots` replace them.

## Loading and lifecycle

Prompts load automatically as instructions, using the existing VFS convention:
all direct `.md` and `.txt` files in each prompt root, sorted by case-sensitive
filename. Numeric prefixes are optional; `instructions.md` has no special
priority. Nested directories are not loaded. Skills publish a catalog; the agent reads selected skill
instructions through the corresponding domain's file tools.

Refresh both sources at eligible idle boundaries, including preparation before
new work, with no active or queued run. Do not scan on model continuations,
command completion, or arbitrary filesystem changes; discovery never wakes an
environment. Keep VFS and environment source ownership separate. Disabling a
source clears only its runtime-owned projection. Switching environments must
remove old-machine prompt instructions and catalogs before new work uses the
new environment. Failure must not masquerade as a successful empty scan or
fall back to another domain; report source unavailability.

## Editor and delivery

- Put **Working directory** directly under VFS and Environments, outside source
  settings. The UI may suggest `/workspace` for a matching VFS link but must
  persist that choice explicitly.
- Provide matching **Prompt loading** and **Skill discovery** switches with
  root overrides behind a secondary **Customize roots** action. The bordered
  rows match Durable jobs and Environment selection tools: label and description
  on the left, switch on the right. Custom-root counts remain visible while
  collapsed. Clearing the field restores defaults; switching off
  removes the block. Explain default directories and full replacement semantics.
- Update engine/API config, runtime adapters, environment prompt publication,
  shared editors, CLI help, and docs. Regenerate API/workflow contracts and all
  consumers. Greenfield configuration cleanup need not retain obsolete fields.
- Cover defaults, overrides excluding home, explicit Claude/Codex paths,
  independent grants, relative tool paths, command overrides, invalid paths,
  environment switching, idle refresh, revocation, and deterministic replay.

## Implementation and validation

Implemented in engine/API configuration, runtime directory resolution, durable job
execution context, environment source scanning, idle prompt publication and
workflow cleanup, and the shared profile/new-session/existing-session editor.
API schemas, TypeScript clients, Configurator tools, and editor reference data
are regenerated. CLI import help and user documentation describe the new shape.
Obsolete nested environment skill scope fields are rejected by the public API.
EnvironmentPromptsConfig and EnvironmentSkillsConfig are separate public and
engine types, matching VFS; only internal root resolution is shared so the
capabilities can evolve independently.

Directory and source tests cover conventional roots, full replacement (including
home), explicit Codex paths, relative reads, invalid directories, prompt ordering,
incomplete scans, bounded unavailable discovery without waking, source revocation,
and environment-switch cleanup with deterministic replay. Durable jobs retain
the session directory in their execution context; per-command overrides remain
local to the command. No database migration or live service reset is required.

Validation includes workspace Rust compilation with tests, component unit suites,
API schema fixtures, frontend type checks, consumer tests, and production builds.
Generated consumers reproduce byte-for-byte. The aggregate npm check's
`git diff --exit-code` guard reports the intended uncommitted generated changes;
its remaining typecheck, consumer-test, and build steps were run separately.
The scoped Rust suites passed 1,010 tests (one prerequisite-dependent test
ignored); consumer tests passed 246 tests, and all 29 focused editor tests passed.
After local-environment authorization, 14 live tests passed, serialized against
PostgreSQL, Temporal, and MinIO: environment provider lifecycle/power (2),
registration/reconnection (1), profiles (2), hosted VFS transfers (1), and
scripted-model session lifecycle/context/checkpoint tests (8). The four
provider-specific model tests were excluded. Prompt/skill source semantics are also covered by the focused filesystem,
schema, editor, and replay tests above.

Prompt discovery now loads direct `.md` and `.txt` files in a single filename
order in both domains. The nested `instructions.d` convention is removed;
existing nested files must move into the prompt root. Environment filename-only
scan patterns prune subdirectories, so ignored trees do not exhaust scan budgets.
The local environment test instruction was moved to the prompt root.

Follow-up validation passed: 8 prompt tests, the environment prompt filesystem
test, 12 test-support tests, 78 daemon tests, 25 focused editor tests, and workspace
compilation. The expanded hosted transfer live test also passed: both VFS and
environment `.md`/`.txt` prompts reached the scripted model in filename order,
with nested and unsupported files excluded. The fixture now configures its
shared gateway for idle discovery as well as tools.

The shared profile/new-session/existing-session editor now presents prompt and
skill capabilities as bordered switch rows matching Durable jobs and Environment
selection tools. Defaults require only the switch; a secondary Customize roots
action reveals root overrides and discovery details without changing
the configuration. Existing overrides are summarized while collapsed. All 30
focused editor tests and the web TypeScript check passed.

The same secondary customization pattern now hides optional sub-agent depth,
descendant count, concurrency, and deadline fields by default. The required
agent selector remains visible, and custom-limit counts stay visible while
collapsed. Expansion does not alter configuration. All 31 focused editor tests
and the web TypeScript check passed.

Web search domain allow/block lists also use a secondary Customize domains action,
with configured counts visible while collapsed. Fetch/search controls remain
visible, and provider-specific exclusivity is preserved. All 33 focused editor
tests and the web TypeScript check passed.

## Environment filesystem and command grants

Environment settings now include independent `tools` (`readOnly` or `edit`,
absent means no file tools) and `commands` (default false) alongside the existing
`jobs` grant. Empty environments no longer implicitly grant file or process tools.
Existing profiles that need the former surface must explicitly set `tools: "edit"`
and `commands: true`; no stored sessions or profiles are silently broadened.

Read-only exposes read/list/search/glob; Edit adds write/edit/patch. Commands
control process start/continuation, and jobs remain independent. Either execution
capability can write files regardless of the filesystem tool setting. Sources
remain independent, including environment reads performed for discovery.

Catalog construction and runtime dispatch enforce the new grants. Runtime
refusals occur before environment resolution or wake. Materialize requires
environment Edit; capture requires environment read access, alongside the
existing VFS source/destination grants and linked-path permissions. Endpoint
capabilities and filesystem permissions remain additional restrictions.

Grant validation passed: the server library suite and focused dispatch tests,
engine, API projection, workflow, and schema tests; workspace test compilation;
258 TypeScript tests; typechecking and production builds. API and workflow
contracts were regenerated, and TypeScript/configurator/reference generation
was verified reproducible. The root `npm run check` stops at its clean-generated-
diff guard while these intended generated changes are uncommitted; its remaining
checks passed separately.

All 14 selected live tests passed serially against local infrastructure:
environment providers/registration, profiles, sessions, and hosted VFS transfers
(including environment prompt loading). Provider-backed model tests were excluded.
The profile provisioning test now accepts either Provisioning or Ready on its
initial read, because a concurrently running development reconciler can complete
provisioning before that read; readiness, ownership, and cleanup checks remain.

Enabling Environments in the shared editor now explicitly selects Edit file
tools, command execution, durable jobs, prompt loading, and skill discovery.
Existing configurations retain their grants; omitted API grants remain off.
Environment selection tools follow skill discovery and remain opt-in.

Enabling VFS in the shared editor likewise selects Edit file tools and enables
prompt loading and skill discovery with default roots. Saved configurations and
workspace link permissions remain unchanged.
