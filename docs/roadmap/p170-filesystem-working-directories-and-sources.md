# P170 — Working directories and prompt/skill sources

Status: proposed, 2026-09-11. Agreed design; implementation pending.

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
`instructions.md` and direct Markdown children of `instructions.d/` in
deterministic order. Skills publish a catalog; the agent reads selected skill
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
  optional root overrides. Clearing the field restores defaults; switching off
  removes the block. Explain default directories and full replacement semantics.
- Update engine/API config, runtime adapters, environment prompt publication,
  shared editors, CLI help, and docs. Regenerate API/workflow contracts and all
  consumers. Greenfield configuration cleanup need not retain obsolete fields.
- Cover defaults, overrides excluding home, explicit Claude/Codex paths,
  independent grants, relative tool paths, command overrides, invalid paths,
  environment switching, idle refresh, revocation, and deterministic replay.
