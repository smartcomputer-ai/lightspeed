# P173 — Session config attachments

**Status:** Implemented across Rust, web consumers, Configurator MCP, and demo
fixtures, 2026-09-13. Follows the workflow-owned preparation work in
[P172](p172-workflow-owned-toolset-reconciliation.md). Greenfield: wire shapes,
engine config types, and stored `ConfigChanged` payloads change in place;
sessions are reset and contracts regenerated. No compatibility aliases.

## Problem

The session config mixes three statements without one grammar:

- **Which resource** is attached: `vfs.workspaceLinks`, `mcp.servers`,
  `subagents.agents`, and for environments only the indirect filters
  `providers` and `registrationKeys`.
- **What the session may do with it**: per-link `access` for VFS, but only
  session-wide `tools`, `commands`, `jobs` for environments, and nothing at
  all for MCP.
- **Which tool surface is presented**: `vfs.tools`, `environments.tools`,
  `selectionTools`. This is where the duplication lives. A workspace link says
  `readWrite` and the feature separately says `edit`; an environment feature
  says `edit` for every machine the session might ever select.

Two concrete limits follow. A session cannot hold two environments with
different access, because the grant is session-wide. And the environment
allowed set is expressed as registry filters rather than as the machines
themselves, which is the only place the config talks about an operator
concept (registration keys) instead of a resource.

## What the research showed

- The asymmetry between environments and everything else is smaller than it
  looks. Environment *policy* already lives inside the config. What lives
  outside is the *active* pointer, and that is runtime state, not config.
  VFS is a namespace (many links at once, disjoint mount points, no fusing),
  an environment is a cursor (one active at a time). Both want an allowed set
  with per-member access in config; only the cursor is state.
- [P95](archive/p95-config-redesign.md) originally kept attachments as
  imperative records outside the config and
  [P107](archive/p107-session-workspace-links.md) reversed that, because a
  profile is precisely the document that says "this agent works on workspace
  X with server Y". Lifting attachments out again would need a second
  document type in profiles and split full-document put across two documents.
  Not revisited.
- MCP record fields fall into three groups. Identity and connection (url,
  label, auth, credential, private network, status) are never per session.
  Transport and presentation (`execution`, `exposure`, `deferLoading`) say
  how tools reach the model, not what a session may do. Only `allowedTools`
  and `approval` have a per-attachment analogue. The allowlist already narrows
  the inventory under both exposures, so a session-level subset is the same
  filter applied once more; search over a narrowed set is pointless but not
  contradictory.
- The current `EnvironmentAccessPolicy` (providers plus registration keys) is
  threaded through the resolver's `selectable` check, the durable job
  execution context, the selection tools, and the listing's key display-name
  grouping. Replacing it with list membership removes all of that.
- The VFS link validator rejects equal paths, `/` beside any other link, and
  prefix relations such as `/data` with `/data/one`. Siblings are fine and
  appear under a synthetic parent. One working directory per VFS domain is
  therefore still right; environments need one per machine.
- Read-only environment file tools do not restrict commands, which can write
  files anyway. That grant combination was a tool-surface preference, not a
  security boundary, and can be folded into an ordered ladder.

## Decision

Every resource-backed feature block is **domain-wide settings plus a list of
attachments**. An attachment is a reference into a universe catalog plus a
domain-specific `access` grant. The presented tool surface is derived from the
union of grants. Nothing else in the config changes shape.

| Feature | Attachment | `access` |
| --- | --- | --- |
| `vfs.workspaces` | `workspaceId` or `snapshotRef` at `path` | `read`, `edit` |
| `environments.environments` | `environmentId` or `inherit` | `read`, `edit`, `exec`, `jobs` |
| `mcp.servers` | `serverId` | optional `tools` subset of the record allowlist |
| `subagents.agents` | `profileId` | none (limits are block-wide and attenuate) |

Ladders are ordered: `edit` implies `read`, `exec` implies `edit`, `jobs`
implies `exec`. Lists are always arrays on the wire; the editor hides that.

### Environments

- The list is the allowed set. Registry filters, registration-key scoping,
  and provider scoping are gone from session config. Selection checks
  membership and a nonterminal record; readiness and wake-on-use stay on
  actual-use paths.
- Each item carries its own `workingDirectory`, falling back to the machine's
  advertised default. Prompt and skill roots stay domain-wide; relative roots
  resolve against each machine's working directory.
- At most one item carries `default: true`. The default **fills an empty
  active pointer at profile application and never overrides a live
  selection**. Creation is the trivial empty case. Applying a profile to an
  existing session first drops an active environment that is no longer
  listed, then activates the default if nothing is active. If the model or
  an API call switched to another listed machine, a profile edit that only
  moves the default does nothing. No default means nothing is activated.
  Plain `session/config/put` never fills the pointer, so a deliberate
  deactivation is not undone by a config edit. This replaces
  `ProfileDocument.environment: existing`; bot exec polls without an explicit
  environment resolve the profile's default item instead of the profile
  intent. Polls and the bot session can therefore diverge: if A and B stay
  listed and the default moves from A to B, the session keeps A while polls
  run on B. That is the price of never overriding a live selection and is
  documented rather than reconciled; a poll that must share the session's
  machine names it with `environmentId`.
  Preparation validates the default's registry state only when it will fill
  the pointer; an unavailable unused default does not block profile updates.
- `inherit: true` is an item kind for sub-agent profiles: the parent's active
  environment at spawn, with the item's own grant. At most one per list.
  Spawning has two stages: the batch executor admits the call and writes a
  `SubagentExecutionContextV1`, and a later preparation activity reads that
  context and resolves the profile. The executor has the parent's active
  environment on its batch request; the context does not carry it today.
  Add `parentActiveEnvironmentId` (nullable) to the context, captured at
  admission, bump its version, and resolve `inherit` from that field in the
  preparation activity. Retries reuse the captured value. The checkpoint
  reread of the parent during child setup is deleted, and a session config
  never contains `inherit`. If the resolved id already
  appears as an explicit item, the explicit item wins and the inherit item is
  dropped. If the parent has no active environment, the inherit item is
  dropped; if it carried `default`, nothing is activated.
- Tools that accept an explicit environment id (`environment_read`,
  `environment_activate`, and job handles on job read and cancel) require
  the id to be a listed attachment, checked like selection. Job handles keep
  their id so a job started on one machine can still be read after
  switching.
- `selection: true` (renamed from `selectionTools`) exposes list, activate,
  and deactivate tools over the list. Harmless with one item.
- `session/start.environment` stays as a creation-time override but must name
  a listed environment; `none` suppresses the default.
- A config put that removes the active environment clears the pointer in the
  same deterministic command. Put already requires an idle session.
- Runtime policy becomes a lookup: the batch carries the active id, and the
  engine derives files, exec, jobs, and working directory from that item.
- **Tool visibility is the union of all items' grants**, installed once.
  A call the active machine's grant does not cover is rejected at execution
  with a message naming the active machine's access. The existing denial
  path covers file and process operations only; durable job submit and run
  are workflow tools whose binding was previously installed only when `jobs`
  was granted. With union visibility that binding exists whenever any item
  grants `jobs`, so the batch executor must check the active item's grant
  before building the job execution context and emitting the workflow
  invocation. The toolset does not change on a switch: a session that moves
  between machines often must not invalidate the provider prompt cache each
  time, and switches happen inside a turn where a patch could not be
  published anyway.
- A new `runtime.catalog.environments` context document lists every
  attachment with id, display name, its access ladder ("access: read, edit,
  exec"), and which one is active. It is built from config and display names
  with no discovery, in the same projection step as the sub-agent menu, and
  shares the `Catalog { title }` shape. `environment_list` and
  `environment_read` results carry the same access line.
  The catalog records its observed selection. Activation, switching,
  deactivation, or removal of the feature invalidates a stale catalog before
  another turn can use it; the next idle runtime projection rebuilds it.

### VFS

- `workspaceLinks` becomes `workspaces`; `access` is `read` or `edit`;
  snapshot links must be `read`. `vfs.tools` is removed. Any link grants read
  tools, any `edit` link grants edit tools, and transfer tools appear when the
  environments feature is granted with matching access on both sides.
- Sourcing prompts or skills from a workspace without file tools is no longer
  expressible. Accepted loss.

### MCP

- Items become `{ serverId, tools? }`. `tools` must be a nonempty subset of the
  record's allowlist and narrows both injection and search. Execution,
  exposure, deferral, approval, and auth remain on the record.
- Rename `approvalDefault` and `deferLoadingDefault` on the record to
  `approval` and `deferLoading`. The suffix promised per-link overrides this
  design does not add.

## Example

Root profile config:

```json
{
  "model": { "providerId": "anthropic", "modelId": "claude-opus-5" },
  "generation": { "reasoningEffort": "high", "parallelToolUse": true },
  "limits": { "maxTurns": 200 },
  "features": {
    "vfs": {
      "workingDirectory": "/workspace",
      "prompts": {},
      "skills": { "roots": ["/workspace/.agents/skills", "/team-skills"] },
      "workspaces": [
        { "path": "/workspace",   "workspaceId": "ws_acorn",         "access": "edit" },
        { "path": "/team-skills", "workspaceId": "ws_shared_skills", "access": "read" },
        { "path": "/ref/v1",      "snapshotRef": "sha256:9f2c…",     "access": "read" }
      ]
    },
    "web": { "fetch": {}, "search": { "allowedDomains": ["docs.rs"] } },
    "environments": {
      "selection": true,
      "prompts": {},
      "skills": { "roots": ["./.agents/skills"] },
      "environments": [
        { "environmentId": "env_ci_runner",     "default": true, "access": "jobs", "workingDirectory": "/srv/acorn" },
        { "environmentId": "env_prod_readonly", "access": "read", "workingDirectory": "/var/log/acorn" }
      ]
    },
    "mcp": {
      "servers": [
        { "serverId": "github" },
        { "serverId": "notion", "tools": ["search", "fetch_page"] }
      ]
    },
    "subagents": {
      "maxDepth": 2, "maxConcurrent": 4,
      "agents": [ { "profileId": "reviewer" }, { "profileId": "log-analyst" } ]
    },
    "timers": {}
  }
}
```

This yields VFS read and edit tools, transfer tools, prompts under all three
links, skills from the two explicit roots, the full environment tool surface
(union of `jobs` and `read`), `env_ci_runner` active at creation with
processes and durable jobs, edit and process calls rejected while
`env_prod_readonly` is active, an environment catalog naming both machines
with their access, the full `github` allowlist, two `notion` tools, and the
two-profile agent menu.

The `reviewer` sub-agent profile config:

```json
{
  "model": { "providerId": "anthropic", "modelId": "claude-sonnet-5" },
  "limits": { "maxTurns": 40 },
  "features": {
    "vfs": {
      "workingDirectory": "/workspace",
      "prompts": {},
      "workspaces": [ { "path": "/workspace", "workspaceId": "ws_acorn", "access": "read" } ]
    },
    "environments": {
      "environments": [ { "inherit": true, "default": true, "access": "exec" } ]
    },
    "mcp": {
      "servers": [ { "serverId": "github", "tools": ["get_pull_request", "list_pull_request_files"] } ]
    },
    "subagents": { "maxDepth": 1, "maxConcurrent": 2, "agents": [ { "profileId": "log-analyst" } ] }
  }
}
```

Spawned from the root above, the parent's spawn activity stores the child
config with `env_ci_runner` in place of `inherit`; setup activates it and
grants processes but not durable jobs. There is no `selection`, so the child
cannot switch.

## Dropped

Never re-propose without new evidence:

- `environments.providers`, `environments.registrationKeys`, registry-scan
  listing, and `EnvironmentAccessPolicy` with its resolver, job-context, and
  selection-tool plumbing.
- `environments.tools`, `environments.commands`, `environments.jobs`,
  `vfs.tools`, `selectionTools`.
- `ProfileDocument.environment`.
- Object-or-array sugar on attachment lists. It costs `anyOf` schemas,
  `T | T[]` client types, and canonicalization for the identical-put no-op,
  and reads would not return what was written.
- Per-session overrides of MCP execution, exposure, deferral, or approval.
- Active-environment tool visibility with toolset patches on switch.
- Defaults that override a live selection on profile application.
- Lifting attachments out of the config into imperative records (the pre-P107
  shape), and feature-level tool caps layered over per-item grants.
- Sourcing-only VFS links.

## Implementation

- [x] Consolidate hosted environment lifecycle, gateway, resolver, runtime, and
  source discovery modules under `crates/temporal-server/src/environments/`.

- [x] Engine config types and validation: attachment lists, ladders, uniqueness,
  one `default`, one `inherit` (profiles only), snapshot links read-only.
  Config replacement clears an active environment that is no longer listed.
  Replay vectors for the new `ConfigChanged` payloads.
- [x] Runtime policy: per-batch environment policy derived from the active item;
  tool reconciler derives VFS, environment, and transfer surfaces from grant
  unions; denial messages name the active machine's access; durable job
  tools gate on the active item's `jobs` grant before invocation;
  explicit-id tools check membership; MCP materialization applies the item subset;
  environment catalog projection.
- [x] Setup and selection: membership-only `selectable`, fill-if-empty default
  at creation and profile application, `parentActiveEnvironmentId` on the
  sub-agent execution context with inherit resolved from it in preparation
  (parent checkpoint reread deleted), bot fire path reads the
  default item, start override validates membership.
- [x] API: DTO renames, MCP record field renames, contract export, TypeScript
  clients, Configurator reference data.
- [x] Editor: attachment rows with an access picker per row; environment rows
  carry default and working directory; MCP rows carry an optional tool
  subset loaded from discovery.
- [x] Web consumers: profile and bot forms use config attachments; session
  activation and creation overrides offer only attached environments. MCP
  forms and gateway mappings use the renamed record fields. Demo profiles
  use the new shapes, reject unlisted activation, and clear removed active
  selections without filling a default on config put.
- [x] Docs: tools-and-mcp, workspaces-and-skills, profiles-and-instructions,
  environments overview, CLI help.

## Acceptance

- Two environments with different grants in one session; the batch policy
  follows the active one, including working directory.
- Config put removing the active environment clears it and does not fill
  the default; profile application fills an empty pointer with the default
  and leaves a live listed selection alone; after a bot profile swaps its
  default from A to B with both listed, the session stays on A and polls run
  on B; put with an unlisted `default` or duplicate ids is rejected.
- Sub-agent spawn captures the parent's active environment (or its absence)
  on the execution context at admission and resolves `inherit` from it
  (explicit item wins, absent parent environment drops the item), storing a
  concrete config; a parent switch after admission or an activity retry does
  not change the child.
- Durable job submit and run are rejected before any workflow invocation
  while the active item grants only `read`, `edit`, or `exec`, and succeed
  under `jobs`.
- Derived tool surfaces match the grant union for every VFS and environment
  combination, including transfer tools; the toolset is unchanged across
  switches, and calls outside the active grant are denied with the access
  named. Explicit ids on read, activate, and job handles are rejected when
  unlisted.
- The environment catalog lists every attachment with its access and marks
  the active one; list and read results carry the same line.
- MCP subset narrows both inject and search exposure; a subset outside the
  record allowlist is rejected at put.
- No session config, profile validation, or runtime path reads providers or
  registration keys.
- Review regressions cover MCP subset materialization for provider injection,
  native injection, and native search; invalid environment IDs returning
  errors; skipping unused default validation; and catalog invalidation,
  stale-publication rejection, and replay across selection changes.
- Contracts regenerated; engine, tools, temporal-workflow, temporal-server,
  profiles, api, and platform checks green.

Review fixes passed 933 unit tests across engine, temporal-server,
temporal-workflow, and tools (one ignored), plus
`cargo check --workspace --all-targets`.

The authorized live run passed 67 ordinary Temporal tests, 43 PostgreSQL
tests, and four OpenAI/Anthropic prompt and skill tests. The Temporal and
PostgreSQL runs preceded the final Rust module reorganization; the provider
tests completed afterward. The VFS sourcing fixture now uses read access on
both attachments. MCP fixtures needed loopback access enabled for the test
process. The separate long-running timeout suite was not rerun.

Web consumer validation passed `npm run check`, including 333 web tests,
TypeScript checks, generated-artifact checks, and live/demo production builds.
Browser verification covered environment attachment rows and MCP discovery,
selection, and the resulting config document.
