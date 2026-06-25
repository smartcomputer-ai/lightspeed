# P85: Agent Profiles

**Status**
- Implemented 2026-06-24.
- Updated 2026-06-25 to reflect the final implemented shape.
- Final shape: profile DTOs, validation, `ProfileSource`, `ProfileError`, and the
  `ProfileStore` trait live in `crates/api/` so every client and runtime boundary
  speaks the same wire types. The earlier separate `crates/agent-profile/` split
  was rejected as unnecessary duplication for this greenfield codebase.
- Builds on **P83 (Fleet Subagent Control Plane)** — `agent_spawn` and the Fleet
  service that compiles agent intent into session/run/resource operations — and
  **P84 (Fleet Wait, Subscriptions, And Send)** — `agent_send`/`agent_wait`. It
  generalizes the messaging-bridge **recipe** concept (`interop/messaging/`,
  `bridge.config.example.json`) into a first-class Lightspeed primitive.
- Closes P83-deferred items: *"Blank/no-source and profile/template-based child
  creation"*, *"`agent_type` / `role` / persona typing and named profiles"*, and
  the P83 Configuration-Model note that *"Named profiles ... are a later
  refinement, not part of v1."*

## Completed V1 Scope

P85 is implemented end-to-end with these concrete pieces:

- **API-owned profile language** in `crates/api/`: `ProfileId`,
  `AgentProfileInput`, `InlineAgentProfile`, stored `AgentProfile`,
  `AgentProfileSummary`, `ProfileDocument`, `ProfileInstructions`,
  `ProfileMount`, `ProfileMcpLink`, `ProfileEnvironment`, tagged
  `ProfileSource`, `AgentProfileUpdatePatch`, typed `ProfileError`, and the
  substrate-neutral `ProfileStore` trait. Profile documents reuse existing
  `api` config/resource input types instead of creating a second dialect.
- **Public JSON-RPC surface**: `profiles/create`, `profiles/read`,
  `profiles/list`, `profiles/update`, `profiles/delete`, `profiles/apply`, plus
  `SessionStartParams.profile`. The committed OpenRPC/schema/method artifacts
  and generated TypeScript client were regenerated.
- **Postgres registry** in `crates/store-pg/`: migration
  `007_agent_profiles.sql` and `PgStore`'s `ProfileStore` implementation with
  optimistic revision checks and document JSONB storage.
- **Hosted runtime applier** in `temporal-server`: resolves named or inline
  `ProfileSource`, merges profile config into `session/start` with explicit
  start config winning at the top level, and applies existing-session profiles in
  order: config, instructions, mounts, MCP, environments. Apply is convergent:
  config, instructions, mounts, MCP links, and environments skip work when the
  effective session state already matches.
- **Fleet integration**: `agent_spawn` accepts `profile`, rejects
  `profile` combined with `source`/fork/isolate options, starts a fresh child
  session from the profile, and records the requested `ProfileSource` in Fleet
  spawn-link metadata.
- **CLI support**: `chat --profile`, `chat --profile-json`, and
  `lightspeed profiles list|read|create|update|delete|apply`.
- **Messaging bridge migration**: bindings use native `profile` values directly
  (named profile id shorthand or explicit `ProfileSource`). Legacy top-level
  `recipes` and `bindings[].recipe` are rejected; session provisioning goes
  through `session/start { profile }` rather than bridge-local
  mount/link/attach loops.
- **Docs and repo index**: `README.md`, `AGENTS.md`, and this roadmap were
  updated to describe profiles as part of the public API/gateway surface.

Verified with:

```bash
cargo fmt
cargo test -p api
cargo test -p tools
cargo test -p cli --tests
cargo test -p store-pg --lib
cargo test -p temporal-server
npm --prefix interop/ts-client run test
npm --prefix interop/ts-client run typecheck
npm --prefix interop/messaging run test:bridge
npm --prefix interop/messaging run typecheck:bridge
npm --prefix interop/messaging run build:bridge
source local/env.sh && cargo test -p temporal-server --test temporal_live temporal_live_profiles_create_start_and_apply_idempotently -- --ignored --exact --nocapture
source local/env.sh && cargo test -p temporal-server --test temporal_live temporal_live_fleet_executor_spawns_profile_child -- --ignored --exact --nocapture
source local/env.sh && cargo test -p temporal-server --test environment_provider_live temporal_live_profile_attaches_host_environment -- --ignored --exact --nocapture
```

## Goal

Today every session inherits its setup from a **source session** (P83 clone/fork)
or, in the bridge, from a JSON **recipe** parsed in TypeScript and applied as a
sequence of API calls. Neither is a first-class Lightspeed concept:

- A clone needs a live source session to copy from. You cannot say "start a fresh
  Anthropic agent with the GitHub MCP and a `/repo` mount" without already having
  one to clone.
- The bridge recipe is the right *shape* (`config` + `mounts` + `mcp` +
  `environments`), but it lives in the bridge, is re-implemented per client, and
  cannot be named by an agent or the CLI.

P85 makes that shape a first-class, named, reusable **agent profile**: a
declarative description of *what an agent is* — provider/model, generation knobs,
tool gates, instructions, VFS mounts, MCP links, environments — that compiles into
the existing session/run/resource operations. Profiles can be:

1. **stored and named** (`profile_id`) in a registry, created/updated/listed/read
   over the API;
2. **referenced** when starting a session (CLI `--profile support`), and when one
   agent spawns another (`agent_spawn { profile: { kind: named, profile_id } }`);
3. **passed ad hoc / inline** at any of those call sites (`--profile-json ...`, or
   `agent_spawn { profile: { kind: inline, profile: {...} } }`), so a profile is
   also a portable **agent-config language** that need not be pre-registered.

(Layered composition — a named base plus an inline override, or profile-to-profile
inheritance — is deliberately **out of v1** and lands additively later; see "Why no
layering in v1".)

The bridge then provisions through native profiles by id (or passes an inline
profile), so there is one profile language and one applier across CLI, bridge,
hosted gateway, and agent-to-agent spawn.

This is deliberately a **superset of the recipe**, not a new orthogonal concept.
The recipe's four sections map 1:1 onto a profile's four sections.

## Design Decision

A **profile is a declarative provisioning document** plus a small amount of
identity metadata. It is *not* a new runtime object: it never has a workflow, a
run, or a session log. It is a recipe for producing one. The profile applier
compiles a profile into the same lower-level operations the bridge and the P83
Fleet service already perform:

```text
profile.config        -> session/start config (or session/update patch)
profile.instructions  -> opening instructions (prompt) injected into the session
profile.mounts        -> vfs/mount/put per mount
profile.mcp           -> session/mcp/link per server
profile.environments  -> session/environments/attach (+ activate)
```

Three layers, deliberately keeping the profile language on the public API
boundary rather than in a separate crate:

1. **`crates/api/`** — owns the `AgentProfile` / `InlineAgentProfile` document
   types, `ProfileSource`, `ProfileId`, validation, typed errors, method DTOs,
   and the substrate-neutral `ProfileStore` trait. This avoids a second config
   dialect and lets CLI, gateway, Fleet tools, and generated clients reuse the
   same contract.
2. **`crates/store-pg/src/profile.rs`** + migration `007_agent_profiles.sql` — the
   Postgres-backed `ProfileStore`. A profile catalog table, exactly like the MCP
   server catalog (`003_mcp.sql`) and environment registry (`006_*`).
3. **profile applier** in the hosted runtime (`temporal-server`) — resolves a
   profile reference to a concrete `AgentProfile`, then applies it against a
   target session through the internal `AgentApiService` and resource stores
   (the same calls the bridge makes today, now in Rust).

`api` gains a thin profile CRUD surface and a `profile` field on the
session-start / spawn paths. The model-visible Fleet surface gains a `profile`
field on `agent_spawn`; it does **not** gain generic profile CRUD tools (an agent
authoring registry entries is deferred — see Deferred).

### Why not just extend P83 clone/fork

Clone/fork answers *"make another agent like this live one"*. Profiles answer
*"make an agent like this **description**"* — no source session required. They are
complementary alternatives: `agent_spawn` keeps `source` (clone/fork a live
session) **and** gains `profile` (instantiate from a description) as a mutually
exclusive choice — each spawn picks one base — and links the result into the graph
the same way regardless.

## The Profile Document

`AgentProfile` is the language. It is a superset of the former bridge recipe
shape, reusing the existing `api` config/resource input types verbatim so there
is no second config dialect:

```text
AgentProfile {
  // ---- identity / metadata (registry-only; ignored for inline profiles) ----
  profile_id            stable id (ProfileId newtype)            [stored only]
  display_name?         human label
  description?          one-line summary
  revision              monotonic; bumped on update              [stored only]

  // ---- the provisioning document (the part that is also valid inline) ----
  config?               SessionConfigInput   (model, generation, context,
                                              run_defaults, tools)
  instructions?         opening instructions text/ref to inject
  mounts?               [ ProfileMount ]     (mountPath, source, access)
  mcp?                  [ ProfileMcpLink ]   (serverId, allowedTools, approval, ...)
  environments?         [ ProfileEnvironment ] (envId, providerId, targetId, activate)
}
```

v1 profiles are **flat** — there is no profile-to-profile inheritance and no
call-site override merging. Composition (`extends` / `compose`) is deliberately
deferred (see Deferred and "Why no layering in v1"); the extension points are
designed in so it lands additively later.

- `config` / `mounts` / `mcp` / `environments` are **the recipe sections,
  unchanged in shape** — they reuse `SessionConfigInput`, the VFS mount-source
  input, the MCP link surface, and the environment-attach input that already exist
  in `api`. Porting a `bridge.config.json` recipe to a profile is a field copy.
- **`instructions`** is the one genuinely new section relative to the recipe. The
  bridge relied on the core discovering `.lightspeed/prompts/` from a mounted
  workspace; a profile can *also* carry inline opening instructions (a persona /
  system framing) so a profile is a complete "agent" without needing a prompt file
  in a mount. This is the seed of the deferred `agent_type` / persona concept —
  it lands as plain instruction text in v1, not a typed role.
- The **document part** (everything except identity metadata) is exactly what an
  **inline** profile carries. Identity fields (`profile_id`, `revision`) are
  meaningful only for stored profiles and are ignored/forbidden on inline ones.

### `ProfileSource`: the universal reference (named | inline)

Every call site that "takes a profile" takes a tagged `ProfileSource`, never a
bare id-or-object union, so a real id and an inline object are never ambiguous
(mirroring P83's tagged `source` and P84's tagged `to`):

```json
{ "kind": "named",  "profile_id": "support" }
{ "kind": "inline", "profile": { "config": { ... }, "mcp": [ ... ] } }
```

- **`named`** — resolve `profile_id` from the registry at the call's current
  revision.
- **`inline`** — an ad-hoc profile document supplied at the call site; never
  touches the registry. This is the "universal agent-config language" use: a CLI
  invocation or an agent spawn can fully describe an agent with no pre-registration.

The same `ProfileSource` type is accepted by `session/start`, the profile-apply
API, and `agent_spawn`. One reference type, every call site. A future `compose`
variant (named base + inline override) is additive — a new enum case, not a
breaking change (Deferred).

### Why no layering in v1

A profile resolves to a concrete document with **no merging**: a `named` source is
the stored document verbatim; an `inline` source is the supplied document verbatim.
v1 deliberately ships **no** profile-to-profile inheritance (`extends`) and **no**
call-site override (`compose`), because section-scoped layering (config patch-merge,
instructions replace, keyed mount/mcp/env unions with a `remove` escape hatch,
`extends` cycle/depth checks) is the most intricate and bug-prone part of the
design, and nothing in the v1 use set needs it:

- "fresh Anthropic agent with different MCPs" → a flat `inline` or `named` profile.
- bridge migration → recipes are already flat documents; they port 1:1.
- "start from `support` but swap the model" → author a second profile (copy +
  edit), a one-time author-side cost rather than runtime layering machinery.

The **one** precedence rule v1 keeps is not general layering: on `session/start`,
an explicit call-site `config` wins over the profile's `config` at the top level
(see "Applying a profile"). That is the minimum needed to let the existing `config`
field coexist with `profile`; it is a single "explicit beats default", not a
recursive merge.

Layering lands later as **`extends`** (profile-on-profile) and a **`compose`**
`ProfileSource` variant, with the section-scoped semantics specified in Deferred.
Because `ProfileSource` is a tagged enum, `extends` is an optional field, and the
stored form already equals the effective form, adding them is purely additive.

## API Surface

### Profile registry CRUD (`api`)

A thin CRUD surface, sibling to `mcp/servers/*` and the environment registry:

```text
profiles/create   { profile }                  -> { profile }
profiles/read     { profile_id }               -> { profile }   (stored document verbatim)
profiles/list     { }                          -> { profiles[] } (compact: id, name, revision)
profiles/update   { profile_id, expected_revision?, patch }  -> { profile }
profiles/delete   { profile_id }               -> { }
```

- `create` / `update` validate the document values (profile ids, non-empty
  labels, duplicate keyed entries, valid mount/mcp/env entries, and reusable
  `api` config/resource input types).
- `read` returns the stored document verbatim. (v1 profiles are flat, so the stored
  form *is* the effective form; when `extends` lands, `read` grows an effective
  view alongside the raw one — Deferred.)
- `update` is optimistic-concurrency-guarded by `expected_revision`, exactly like
  `session/update`'s `expected_config_revision`.

### Applying a profile to a session

Two entry points, both compiling a `ProfileSource` to the lower-level ops:

- **At session creation** — `session/start` gains an optional `profile:
  ProfileSource`. When present, the gateway resolves+applies it (config →
  start-config, then mounts/mcp/environments/instructions) as part of bringing the
  session up. If the same `session/start` also carries an explicit `config`, the
  explicit `config` **wins at the top level** over the profile's `config` (the one
  precedence rule v1 keeps — not recursive merging; see "Why no layering"), so the
  existing `config` field keeps working unchanged.
- **To an existing session** — `profiles/apply { session_id, profile,
  expected_*_revision? }`. This is the bridge's `ensureSession` made native and
  idempotent: it diffs desired-vs-current and issues only the needed
  mount/link/attach calls. Re-applying the same profile is a no-op (the bridge's
  `startedSessions` guard becomes a real idempotent apply).

Application is **idempotent and ordered**: config first (so tool gates exist),
then instructions, then mounts, then mcp links, then environments (attach +
at-most-one activate) — the same order the bridge uses, with each step keyed so a
retry after partial application converges.

### CLI

```bash
# start a session from a named profile
cargo run -p cli -- chat --new --profile support "help with the ticket"
# inline / ad-hoc profile from a JSON file or literal
cargo run -p cli -- chat --new --profile-json ./anthropic-coder.json "..."
# manage the registry
cargo run -p cli -- profiles list
cargo run -p cli -- profiles create ./support.json
cargo run -p cli -- profiles read support
```

`--profile` builds a `named` `ProfileSource` and `--profile-json` an `inline` one;
both pass to `session/start`. (A `--profile-set key=value` override flag arrives
with the deferred `compose` variant.)

### Fleet (`agent_spawn`)

`agent_spawn` gains an optional `profile: ProfileSource`, making profile-based
children first-class (the P83-deferred "profile/template-based child creation"):

```text
agent_spawn {
  child_session_id?
  input
  source?     tagged source enum   (P83: self | session)   -- clone/fork a LIVE session
  profile?    ProfileSource        (P85: named | inline)    -- instantiate a DESCRIPTION
  ...
}
```

`source` and `profile` are **mutually exclusive** — a spawn instantiates a child
*either* from a live session (clone/fork) *or* from a description (profile), never
both. Layering a profile over a clone is deliberately not offered: the two derive
overlapping setup (config, mounts, mcp, environments) from different bases, so a
combined order-of-precedence is exactly the "tricky overlay" worth avoiding. The
three cases are:

- **Neither** → P83 default: clone `{ kind: self }` (today's behavior unchanged).
- **`profile` only** → fresh session provisioned purely from the profile document
  (no source session needed). This is the "OAI agent starts a fresh Anthropic
  agent with different MCPs" case the goal calls out.
- **`source` only** → P83 clone/fork (unchanged).
- supplying **both** is a validation error (`source` and `profile` are mutually
  exclusive).

"Fork myself but switch provider" is **not a v1 one-shot** (it would need
profile-on-live-session overlay, the undefined precedence we are avoiding). In v1
the caller spawns from a `profile` that already describes the target setup, or
clones via `source` and adjusts the child afterward (`agent_send` / a follow-up
`profiles/apply`). Once the deferred profile layering lands, the intent collapses
into a single `compose`/`extends` profile — profile-on-profile, never
profile-on-live-session. Clone/fork stays a pure copy of a live session.

Either way the spawned agent is still an ordinary linked Fleet child (the
parent→child link is created exactly as in P83); `profile` only changes how its
*setup* is derived.

`agent_spawn` accepts `inline` and `named` profiles, so a supervisor can hand a
fully ad-hoc agent description to a child without anything being registered first —
the agent-config-language goal at the agent-to-agent boundary.

## Identity / Metadata (the seed of `agent_type`)

A profile's `display_name` / `description` / `instructions` are the first durable
place an "agent kind" is described. v1 keeps this as **plain metadata + instruction
text**, not a typed role system:

- there is no `agent_type` enum, no role namespace, no persona object;
- Fleet children provisioned from a profile record the requested `ProfileSource`
  in the Fleet spawn-link metadata, so `agent_read` can explain profile-based
  child creation without a typed role model. A general session-level
  `profile_id`/revision lineage field is deferred.

This deliberately mirrors P83's identity stance (`agent_id == session_id`, product
typing deferred). Profiles give the *eventual* `agent_type` a home to grow into
without committing to the taxonomy now.

## Secrets / Auth

Profiles never store secrets or resolved credentials — same rule as P83 clone.
`mcp` / `environments` entries reference `serverId` / `grantId` / `providerId` /
`targetId`; tokens are minted at call time by the existing broker. A profile is
safe to store in the registry and to pass inline (e.g. over the bridge config)
because it is references-only.

A profile naming an MCP server / environment / grant that does not exist (or the
caller cannot reach) fails **at apply time** with a clear per-entry error, not at
profile-create time — the registry stores intent; reachability is a runtime
property (mirrors P83 trusting a named source id, P84 `not_reachable`).

## Migrating The Bridge Off Recipes

The bridge recipe and the profile document are the same shape, so migration is
mechanical and staged:

1. **Land profiles** (registry + applier + `session/start` `profile`).
2. **Bridge references profiles**: `bindings[].profile` is a `ProfileSource`
   (with string shorthand for a named profile id). A binding can name a
   registered profile **or** carry an inline one. Existing `recipes` and
   `bindings[].recipe` config were removed rather than kept as a compatibility
   shim.
3. **Delete `ensureSession`'s per-step apply** from `interop/messaging/`; the
   bridge calls `session/start { profile }` and lets the native applier do the
   mount/link/attach sequence. The bridge keeps only its *binding/match* logic
   (channel→profile+sessionKey), which is bridge-specific and stays.

The bridge's old "default recipe = messaging tool only" behavior remains only as
the unprofiled conversation default: when no binding profile is configured, the
bridge starts the session with `tools.messaging = true`.

## Crash / Idempotency / Determinism

- The registry is plain CRUD storage; no determinism concern (it is read by the
  applier, never by `engine`).
- **Apply is idempotent and convergent**: keyed per mount/link/environment so a
  retry (tool-activity retry on `agent_spawn`, bridge retry, gateway restart
  mid-apply) re-issues only missing steps. This is the same property the bridge's
  old `startedSessions` set approximated, made real.
- For `agent_spawn`, application happens in the **Fleet tool activity / hosted
  runtime** (outside `engine`), exactly where P83 already does clone/fork/link
  side effects. The engine learns nothing about profiles. The spawned child run's
  `submission_id` is still derived from the parent tool identity (P83), so spawn
  retries do not double-provision or double-admit.
- A profile applied at `session/start` is applied **before the first run is
  admitted**, so the session's opening config/resources are in place for turn one
  (the ordering guarantee the bridge used to provide with `ensureSession`, now in
  the hosted runtime).

## Implementation Map

- `crates/api/`: `AgentProfile`, `InlineAgentProfile`, `ProfileId`,
  `ProfileSource` (`named | inline`), section types reusing existing `api`
  config/resource inputs, `validate()`, typed `ProfileError`, `ProfileStore`
  trait, profile CRUD/apply DTOs, `profiles/*` JSON-RPC methods, and optional
  `SessionStartParams.profile`. No layering/`extends` in v1. Regenerated committed
  contract artifacts (`cargo run -p api --bin export-schema`).
- `crates/store-pg/src/profile.rs` + `migrations/007_agent_profiles.sql`:
  Postgres `ProfileStore` impl and an `agent_profiles` catalog table (id,
  display_name, description, revision, document JSONB, timestamps). (Pattern:
  `mcp.rs` / `003_mcp.sql`.)
- `crates/temporal-server/`:
  - **Profile applier**: resolve a `ProfileSource` (registry lookup for `named`,
    verbatim for `inline`; no merging), then apply to a session via internal
    `AgentApiService` + VFS/MCP/environment stores in the canonical order,
    idempotently. Used by the `session/start` `profile` path, `profiles/apply`, and
    the Fleet executor.
  - `gateway`: route the `profiles/*` methods to the store; route `session/start`'s
    `profile` to the applier before first-run admission.
  - `fleet.rs`: `agent_spawn` starts a fresh session from `profile` (rejecting
    `source` + `profile` together) before child run admission and records the
    `ProfileSource` on the Fleet spawn-link metadata.
- `crates/cli/`: `--profile` (named) / `--profile-json` (inline) on `chat`, and a
  `profiles` subcommand (`list|read|create|update|delete|apply`).
- `interop/messaging/`: accept a `ProfileSource` per binding (plus string
  shorthand for named profiles); reject legacy `recipes` / `bindings[].recipe`;
  deleted `ensureSession`'s manual mount/link/attach loop in favor of
  `session/start { profile }`; keep binding/match logic.

## Implementation Steps Completed

### S1. Profile language in `api`
- Completed in `crates/api/`: `AgentProfile`, `InlineAgentProfile`, `ProfileSource`
  (`named | inline`), section types, `validate()`, and `ProfileStore` trait. No
  layering. JSON-RPC routing and schema export coverage live in `cargo test -p
  api`.

### S2. Registry storage + API CRUD
- Completed: `store-pg` `agent_profiles` table + `ProfileStore` impl; `api`
  `profiles/create|read|list|update|delete` DTOs/methods with optimistic
  concurrency on `update`; `read` returns the stored document verbatim. Regenerated
  contract artifacts.

### S3. Profile applier + `session/start { profile }` + `profiles/apply`
- Completed: hosted applier resolves `ProfileSource` (registry for `named`,
  verbatim for `inline`; no merging), applies in canonical order idempotently,
  wires `session/start`'s optional `profile` (with an explicit call-site `config`
  winning at the top level), and supports `profiles/apply { session_id }`.
  Apply-time resource failures are surfaced through the existing
  VFS/MCP/environment paths.

### S4. Fleet `agent_spawn { profile }`
- Completed: `agent_spawn` accepts `profile` (named/inline), **mutually exclusive
  with `source`**; `profile`-only spawns a fresh provisioned child; Fleet spawn-link
  metadata records the requested `ProfileSource`; idempotent under tool-activity
  retry. Tests cover profile-only spawn and `source`+`profile` rejection.

### S5. CLI
- Completed: `--profile` (named) / `--profile-json` (inline) on `chat`; `profiles`
  subcommand. CLI parse tests cover chat profile flags and profile apply.

### S6. Bridge migration
- Completed: recipe application was replaced with `ProfileSource` per binding;
  reusable bridge setup now lives in the profile registry or as an inline
  `bindings[].profile`; legacy `recipes` and `bindings[].recipe` are rejected;
  the manual mount/link/attach apply loop was deleted. Bridge docs, example
  config, and tests were updated.

## Deferred

- **Model-visible profile CRUD tools** (`profile_create`/`profile_list` for
  agents). v1 lets an agent *reference* a profile in `agent_spawn` (named or
  inline) but not *author registry entries*; an agent that wants a custom child
  passes an `inline` profile. Authoring registry entries from an agent waits for a
  policy/ownership story.
- **Profile layering — `extends` and a `compose` `ProfileSource` variant.** v1
  profiles are flat (the stored/inline document is applied verbatim). Layering
  lands later with section-scoped semantics defined once and shared by both:
  `config` deep-merges via the existing `SessionConfigPatchInput` semantics;
  `instructions` replaces (with a possible append mode); `mounts`/`mcp`/
  `environments` are keyed unions (key = `mountPath`/`serverId`/`envId`) with a
  per-section `remove` escape hatch; `extends` is acyclic and depth-bounded, and
  `profiles/read` then returns raw + effective. Additive: `ProfileSource` is a
  tagged enum, `extends` is an optional field, and v1's stored form already equals
  its effective form. This is what makes "fork-self-but-switch-provider" a one-shot
  (`compose`) and DRY base/variant profiles (`extends`) — neither needed for the v1
  use set, and both the most bug-prone part of the design.
- **Typed `agent_type` / role / persona taxonomy.** v1 carries persona as
  `instructions` text + `display_name`; a typed role model layers on later.
- **`environment = isolate` and richer resource isolation in a profile.** Inherits
  P83's deferral; a profile can request `share` (and `vfs` isolate where P83
  already supports it) but not new environment isolation.
- **Per-grant / less-privileged profiles.** A profile cannot yet scope auth down
  (needs the P83-deferred principal/grant-selection machinery); it references the
  same `universe_default` grants.
- **Profile versioning/pinning beyond `revision`.** v1 resolves a `named` profile
  at its current revision; pinning a session to a specific historical revision (vs
  always-latest) is a later refinement. General session-level profile lineage,
  including a stored `profile_id` + revision for `session/start`, is also
  deferred; Fleet profile spawns currently record the requested `ProfileSource`
  on the spawn-link metadata.
- **Profile ownership / sharing / namespacing** across users or workspaces.
- **Validation against live resource existence at create time** (v1 validates
  shape at create, reachability at apply).

## Acceptance Criteria

- A profile is a first-class, named registry object
  (`profiles/create|read|list|update|delete`) whose document is a **superset of the
  bridge recipe** (`config` + `mounts` + `mcp` + `environments`, plus
  `instructions`), reusing existing `api` config/resource input types — no second
  config dialect.
- Every profile-taking call site accepts one tagged **`ProfileSource`** —
  `named` | `inline` — so a profile can be stored or passed ad hoc identically from
  the CLI, `session/start`, and `agent_spawn`. A profile resolves to a concrete
  document with **no merging** (v1 is flat); the deferred `compose` variant is
  additive.
- v1 ships **no profile layering**: a `named` source applies the stored document
  verbatim and an `inline` source applies the supplied document verbatim. The one
  precedence rule kept is that an explicit call-site `config` on `session/start`
  wins over the profile's `config` at the top level — not recursive merging.
  `extends`/`compose` layering is Deferred.
- `session/start { profile }` provisions a session **before the first run** in the
  canonical order (config → instructions → mounts → mcp → environments), and
  `profiles/apply` does the same to an existing session **idempotently** (re-apply
  is a no-op), replacing the bridge's `ensureSession`/`startedSessions` logic.
- `agent_spawn` gains an optional `profile`, **mutually exclusive with `source`**:
  `profile`-only spawns a fresh provisioned child with **no source session
  required** (e.g. an OAI agent spawning a fresh Anthropic agent with different
  MCPs); supplying both is a validation error. The result is still a linked Fleet
  child; retries do not double-provision. ("Fork-but-switch-provider" is not a v1
  one-shot — it needs the deferred profile layering — and is handled meanwhile by a
  profile that already describes the target or a clone adjusted afterward.)
- Profiles store **references only**, never secrets; a missing/unreachable
  referenced resource fails **at apply time** with a per-entry error.
- The **messaging bridge provisions through native profiles** (named id shorthand
  or inline `ProfileSource`). Legacy recipe JSON is rejected instead of converted;
  the bridge's manual mount/link/attach provisioning loop is gone.
- No profile logic lives in `engine`; the applier runs in the hosted runtime
  exactly where P83 performs clone/fork/link side effects.
- Fleet children provisioned from a profile record the requested `ProfileSource`
  in spawn-link metadata so `agent_read` can explain profile-based child
  creation, seeding the deferred `agent_type` concept without a typed role model.
  General session-level profile lineage is deferred.
