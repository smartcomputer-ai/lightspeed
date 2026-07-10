# P95: Config Redesign — Full-Document Puts, Feature-Oriented Session Config

**Status**
- Proposed 2026-07-09.
- Slice 1 completed 2026-07-09: engine config core restructured
  (`SessionConfig { model, generation, limits, context, features }`, patch
  types deleted, `ReplaceSessionConfig` with identical-doc no-op, feature
  versions pinned via serde, `ToolChoice` flattened to a bare enum with
  `parallel_tool_use` lifted, `reasoning_effort` as an opaque string,
  `effective_generation` planning overlay, `LlmRequest` carries
  `reasoning_effort`/`parallel_tool_use` in the fingerprint).
- Slice 2 completed 2026-07-09: workspace alignment + API cutover. Wire
  `SessionConfig` doc (deny_unknown_fields) + `session/config/put` replace
  `SessionConfigInput`/`session/update`/all patch DTOs; materialized config
  views deleted (`SessionView.config` = sparse doc); `profiles/update`
  deleted end-to-end (api, profiles crate ext/`UpdateAgentProfile`,
  store-pg, gateway) — pulled forward from the registry slice because its
  patch embedded the old input type; gateway translation rewritten
  (`engine_session_config_from_api`), put handler with gateway-side
  identical-doc short-circuit, feature→toolset materialization (fleet grant
  implies concurrency tools), reasoning-effort tier validation at admission;
  reasoning/parallel materialization moved into `llm-runtime` adapters
  (params-precedence, per-provider tier validation); secure-by-default flip
  live (`default_session_config` = model only); CLI sends explicit dev
  features (vfs edit + prompts/skills sourcing + web + timers) with
  `--bare`; profile apply = full-document put; contract artifacts + TS
  client regenerated (`FieldPatchOf*` reduced to the MCP-update remnant).
  All unit suites green incl. LLM live suites and the local
  Temporal/Postgres live stack.
- Slice 3 completed 2026-07-09: declarative MCP + derived toolset. The
  session toolset is now level-triggered derived state
  (`toolset_reconcile_patch` converges installed tools — standard AND remote
  MCP — to what the current config implies; RemoteMcp is no longer exempt
  from the diff). `features.mcp.servers` resolve against the catalog + auth
  grants at put/start/apply admission (`desired_mcp_tools`; bad links fail
  before the document enters the log) and materialize during
  `configure_session_toolset`. Removed end-to-end: `session/tools/update`,
  `session/mcp/link`, `session/mcp/unlink` (RPC, DTOs, gateway `tools_api`
  module, link patch helpers), `ProfileDocument.mcp`/`ProfileMcpLink` (+
  profiles-crate validation, gateway apply step, `mcp_changed` summary
  counter) — profiles declare MCP via `config.features.mcp`. Kept:
  `session/mcp/list` (observability over materialized links; 86 methods
  total). CLI `mcp link`/`unlink` became read-modify-put sugar over
  `features.mcp` (unlink now takes the server id, not a tool id). Per-link
  `tool_id`/`server_label` overrides were dropped — the tool name derives
  from the server id. Contract artifacts + TS client regenerated.
- Slice 4 completed 2026-07-09: MCP catalog converged on put.
  `McpServerRecord` gained `revision`; `mcp/servers/create` +
  `mcp/servers/update` replaced by `mcp/servers/put`
  (`McpServerInput` full document + optional `expectedRevision`, exact
  `profiles/put` semantics incl. `McpRegistryError::RevisionConflict`).
  Store trait `put_server` (create-or-replace, `created_at_ms` preserved,
  CAS revision guard in store-pg; `003_mcp.sql` edited in place). Deleted:
  `CreateMcpServerRecord`→`PutMcpServerRecord` rename,
  `UpdateMcpServerRecord`/`apply_to`, `McpServerUpdatePatch`, and
  `FieldPatch<T>` (zero remaining users; `FieldPatchOf*` gone from the TS
  client). CLI `mcp server add`/`update` became `mcp server put`
  (`--expected-revision`). 84 methods total; contract artifacts + TS client
  regenerated.
- Slice 5 completed 2026-07-09: `session/messages/submit` RPC surface removed
  (constant, route, service method, `MessageSubmitParams`/`Response`, gateway
  handler, schema-artifact coverage, hand-written TS `submitMessage` wrapper +
  `SubmitMessageOptions`). The engine `SubmitMessage` command and mailbox
  machinery stay — load-bearing for fleet `agent_send` and detached-promise
  follow-ups; an ACP/A2A adapter can reintroduce an external surface later.
- Slice 6 completed 2026-07-09: docs and examples. `docs/design.md` describes
  the sparse capability config + derived toolset; `AGENTS.md` gained the
  config architecture rule (no field-level patch vocabulary; registries use
  put-with-expected-revision; toolset is derived) and the profiles-crate
  description dropped update records; `docs/roadmap/roadmap.md` marks P95
  done; the runnable example profiles under `profiles/` were ported to the
  feature-config shape (mcp-echo declares its server via
  `config.features.mcp`).
- Follow-up 2026-07-09: `session/mcp/list` removed as well — it was a strict
  subset projection of `SessionView.activeTools` (whose
  `ToolKindView::RemoteMcp` carries label/url/allowed-tools/approval/
  defer-loading/auth-ref); declaration is `config.features.mcp`,
  materialization is `activeTools`. `SessionMcpLinkView` deleted
  (`SecretRefView` stays for the tool view); CLI `mcp list` and link/unlink
  output read `activeTools`.
- Follow-up 2026-07-09: §5 authorization enforced. Resource verbs now check
  their granting feature (`session/mounts/put` requires `features.vfs`;
  `session/environments/create`/`attach` require `features.environments` —
  rejected otherwise), and `session/config/put` refuses to revoke a feature
  while dependent bindings are live (`vfs` with mounts present,
  `environments` with non-detached bindings — conflict listing the bindings;
  teardown stays explicit). Live suites re-validated with grants added to
  their session configs.
- Follow-up 2026-07-10: runtime projection now honors the sparse feature
  grants end to end. VFS and environment catalogs are published only while
  `features.vfs` and `features.environments` are granted (and stale managed
  entries are removed on revocation). Prompt and skill discovery are gated by
  `features.vfs.prompts` / `features.vfs.skills`, including their optional
  explicit root lists, across the gateway, Temporal pre-run refresh, and the
  in-process test runner.
- Follow-up 2026-07-09: `session/prompts/active` removed; context entries
  unified on one faithful view. `SessionItemView` (display-oriented, lossy:
  keys dropped, kinds collapsed to `SystemEvent`, refs missing) replaced by a
  `ContextEntryView` envelope — `id`, `key`, `kind` (`ContextEntryKindView`),
  `contentRef`, `mediaType`, `preview`, `providerKind`, `providerItemId`,
  `tokenEstimate`, plus `text` (blob text inlined only for message/tool
  entries) and `display` (provider-executed tool summary) — used uniformly in
  `SessionView.activeContext.entries`, `RunView.entries`, and the context
  session events (`items`/`itemIds` fields renamed `entries`/`entryIds`).
  Context entry keys are wire contract: clients reconstruct the prompted
  instruction set by filtering the `prompt_instructions/` key prefix and
  fetch bodies/reports via `blobs/read` (the assembly report ref rides
  `providerItemId`, as stored). Gateway `project_active_prompts`/report
  helpers deleted; idle-session prompt refresh at run-start boundaries is
  unchanged. Dead `session/items/completed` notification (`ItemCompleted`,
  zero emitters) removed with it. CLI renders from the envelope; fleet
  `agent_read` transcript extraction reads the new shape.
- **P95 is complete.** Final wire surface: 82 methods; zero patch vocabulary
  anywhere in `crates/api`.
- **Greenfield: breaking changes are fine.** No migrations, no compat shims, no
  deprecated aliases. Wire shapes, engine config types, and stored
  `ConfigChanged` payloads change in place; local stacks are reset and
  `interop/contract/` + the TS client are regenerated.
- Replaces the patch-based config update model (`FieldPatch`,
  `SessionConfigPatchInput`, engine `SessionConfigPatch`) with full-document
  puts guarded by expected revisions, and restructures the session config
  around features with capability (default-off, grant-to-enable) semantics.
- Companion cleanups: `profiles/update` and `mcp/servers/update` fold into
  their `put` counterparts; the unused `session/messages/submit` RPC surface is
  removed.
- Sequencing: engine config core first — get the structures right while the
  rest of the workspace is allowed to break; one alignment slice then fixes
  every downstream crate and cuts the wire surface over. No temporary shims,
  no behavior-preserving intermediate states.

## Goal

1. **One update semantic.** Documents are replaced whole, guarded by
   `expected_revision`. Read-modify-write replaces field-level patch ops
   everywhere a config-like document is edited. `profiles/put` already
   implements the target pattern (`crates/api/src/profiles.rs:321-329`,
   `crates/store-pg/src/profile.rs:158-181`); everything else converges on it.
2. **Sparse, feature-oriented config.** The session config document contains a
   small core-machinery part (model, generation, limits, context) and a
   `features` part where each entry is a capability grant: absent = off,
   present = on with sensible defaults. Only deviations from defaults are
   stored.
3. **Secure by default.** The default session is a model that can process
   runs — no tools, no filesystem, no network, no environments. Everything
   world-touching is an explicitly granted feature.
4. **Tools are implementations, not config surface.** The session toolset
   becomes derived state: a function of (feature config, provider, linked
   resources). Clients configure features; the gateway materializes tools.
5. **Feature versioning.** Each feature block carries a version so breaking
   behavior changes can ship as a new version while existing sessions stay
   pinned.

## Current state (2026-07-09, branch `concurrency-pr`)

### Session config and its patch machinery

- Engine `SessionConfig { model, run, turn, context, tools, fleet }` —
  `crates/engine/src/core/components/config.rs:10-20`. Stored fully inside
  `ConfigChanged { config, revision }` lifecycle events
  (`core/components/lifecycle.rs:11-14`); `config_revision` starts at 0 and is
  bumped with `checked_add(1)` at admission (`core/admit.rs:38-83`).
- Wire input `SessionConfigInput { model, generation, context, run_defaults,
  tools, fleet }` (`crates/api/src/sessions.rs:15-30`), patched via
  `session/update` (`SessionUpdateParams` with `expected_config_revision` +
  `SessionConfigPatchInput`, `sessions.rs:172-197`) built on
  `FieldPatch<T> = Set(T) | Clear` (`sessions.rs:199-214`). The engine mirrors
  the whole patch tree (`SessionConfigPatch`, `OptionalConfigPatch<T>`,
  `config.rs:46-252`), and the gateway translates between the two
  (`temporal-server/src/gateway/service/api_config.rs`).
- Defaults are applied in three places: `default_session_config` seeds the base
  at open (`temporal-workflow/src/config.rs:35-48`, `DEFAULT_MODEL`), narrow
  serde defaults on sub-structs, and lazy `effective_*` helpers at projection
  and toolset-build time (`api-projection/src/lib.rs:1206-1228`, duplicated at
  `gateway/service/mod.rs:1350-1364`). Today `web_search`, `web_fetch` default
  **on** and `filesystem` defaults to **edit** — the opposite of secure by
  default. `ToolConfigView` also drifted (no `messaging` field,
  `api/src/views.rs:43-51`).
- Reasoning effort is not stored as intent: the gateway elaborates it into
  provider-native `provider_params` JSON that lands in the session log
  (`api_config.rs:407-451`), even though provider vocabulary is supposed to
  stay out of the engine.

### Patch/partial-update surfaces across the API

| Surface | Method | Representation | Expected revision |
|---|---|---|---|
| Session config | `session/update` | nested `FieldPatch` structs | yes (gateway + engine) |
| Session tools | `session/tools/update` | `Replace` \| `Patch{upsert,remove}` | yes |
| Session MCP link | `session/mcp/link`/`unlink` | synthesized engine `ToolPatch` | yes (tools revision) |
| Profiles | `profiles/update` | `AgentProfileUpdatePatch` (FieldPatch scalars, replace-collections) | yes |
| Profiles | `profiles/put` | **full document** ✅ target model | yes |
| MCP catalog | `mcp/servers/update` | `McpServerUpdatePatch` | **no — last write wins** |
| VFS workspace | `vfs/workspaces/update` | targeted put (snapshot ref + name) | yes |
| Context | `session/context/append` | keyed idempotent upsert | n/a (by design) |
| Env providers | `environments/providers/register` | SQL upsert | no |
| Auth grants | `auth/grants/import` | insert-or-ignore | no |

### Feature config split (ad hoc today)

Simple on/off flags (`web_search`, `web_fetch`, `messaging`, `fleet`, `timer`)
and the `filesystem` tri-state live flat under `tools`
(`config.rs:336-356`); fleet *policy* has its own top-level `fleet` section
(`config.rs:358-364`); compaction lives under `context`; rich per-feature
config (web-search domains/context-size/location,
`tools/src/web/search.rs:17-31`) exists only in the internal `ToolsetConfig`
materialization layer (`gateway/service/mod.rs:914-951`) and is not
client-configurable; prompts, skills, VFS mounts, MCP links, and environments
sit entirely outside `SessionConfig` as per-session resources.

## Design

### 1. One sparse config document

The session config is a single document with two kinds of sections:

- **Core machinery** — how the agent runs, always present conceptually:
  `model`, `generation`, `limits`, `context`.
- **Features** — what the agent is allowed to touch and the static rules for
  it, default-off capability grants: `vfs`, `web`, `messaging`, `fleet`,
  `timers`, `environments`, `mcp`.

```jsonc
{
  "model": { "providerId": "openai", "apiKind": "openai_responses", "model": "gpt-5.5" },
  "generation": { "maxOutputTokens": 32000, "reasoningEffort": "high" },
  "limits": { "maxTurns": 40, "maxToolRounds": 200 },
  "context": { "compaction": { "mode": "provider_triggered" } },
  "features": {
    "vfs": {
      "version": 1,
      "tools": "edit",
      "prompts": {},
      "skills": { "roots": ["/skills"] }
    },
    "web": { "version": 1, "fetch": {}, "search": { "allowedDomains": ["docs.rs"] } },
    "messaging": { "version": 1 },
    "fleet": { "version": 1, "profiles": { "allow": ["researcher"] } },
    "timers": { "version": 1 },
    "environments": { "version": 1 },
    "mcp": { "version": 1, "servers": [{ "serverId": "linear" }] }
  }
}
```

Semantics:

- A feature key **absent** means the capability is not granted: no tools, no
  access. `{}` (or `{ "version": 1 }`) means granted with defaults. This
  replaces `Option<bool>` flags and `FilesystemToolMode::None`.
- Within a granted feature, omitted fields take that feature's defaults.
  Sub-features follow the same presence rule (`web.fetch` and `web.search` are
  independently granted; `"web": {}` grants neither — validation rejects an
  empty `web` block as meaningless).
- The stored document is the **sparse document itself** (engine `SessionConfig`
  is restructured to mirror these sections, all-`Option` where sparse).
  Admission materializes exactly two things into the stored doc: `model`
  (deployment default stamped at open; `api_kind` stays pinned per session as
  today) and feature `version` fields (§6). Everything else stays sparse, so
  reads round-trip cleanly for read-modify-write.
- Wire DTOs use `deny_unknown_fields` so typos fail validation instead of
  silently disabling a capability.

Why `features` is a struct-of-options, not a map, and why it is not a
separate document (considered 2026-07-09):

- On the wire a struct of `Option` fields already serializes as a keyed
  object — features are individually addressable and the JSON reads as a
  registry. A true `Map<FeatureId, Value>` in Rust would trade exhaustive
  compile-time matching in the materialization code and unknown-key
  *rejection* (capability semantics require the server to understand every
  grant it accepts) for open-world extensibility this product deliberately
  does not have (P91 closed-vocabulary posture).
- Documents split along **writer and lifecycle boundaries, not schema
  taxonomy**. Session config has one writer role (operator/profile), one
  cadence (idle-only, rare), and cross-section validation (`web.search`
  depends on `model.api_kind`; `tool_choice` references feature-materialized
  tools) — separate per-feature revisions would leave "what revision is this
  session's config" unanswerable and make profile apply multi-document.
- Per-feature *addressability* is ergonomics, addable later as sugar
  (`session/features/put` writing the same document under the same revision);
  per-feature *storage* is an architecture commitment. Take the first when
  needed, not the second.
- Triggers that would justify a future split, for the record: a feature
  needing mid-session writes (graduates to a resource per §5, not to a
  sibling config doc); a section owned by a different principal (e.g. tenant
  policy locking fleet/spawn — that becomes a policy overlay document owned
  by that principal, split by writer again).

The engine `run`/`turn` split disappears from session config: `generation`
covers turn-shaping (`maxOutputTokens`, `reasoningEffort`, `toolChoice` —
flattened to the bare choice enum — and `parallelToolUse`, lifted out of the
old `ToolChoice` wrapper where it sat as Anthropic request shape),
`limits` covers run budgets (`maxTurns`, `maxToolRounds`). Per-run overrides
continue to ride `RunStartConfig` on `session/runs/start`; session-level
`model_override` and stored `provider_params` are dropped. `reasoningEffort`
becomes a stored, provider-native tier **string** (e.g. `high`, `xhigh`,
`ultra`) that the engine carries opaquely — no engine enum chasing provider
tier vocabulary; the LLM runtime validates the value against the provider and
materializes the request params. This moves the elaboration out of the
gateway (`api_config.rs:407-451`) and gets provider JSON out of the session
log.

The `vfs` feature grants the virtual filesystem itself (mounts attachable,
catalog surfaced) with three orthogonal sub-grants: `tools` (agent fs tool
surface — `read_only`/`edit`, absent = no fs tools), `prompts` and `skills`
(sourcing roots). `"vfs": {}` meaningfully grants a VFS with no tools and no
sourcing. Two access layers compose by intersection and must not be
conflated: `tools` shapes which fs tools exist in the agent's toolset (a
stable capability ceiling, and prompt-visible surface), while per-path
writability is defined and enforced by each mount's own access
(`readOnly`/`readWrite` — data-layer permission, per session, mutable with
the mount topology). Sourcing from mounted environments is a later,
environment-specific concern that rides environment bindings, not this
block.

### 2. Full-document put with expected revision

- New method **`session/config/put`** replaces `session/update`:
  `{ sessionId, expectedConfigRevision?, config }` → updated `SessionView`.
  The document replaces the previous config wholesale; anything omitted
  reverts to default/off. `expectedConfigRevision` follows the uniform
  convention (`None` = unconditional; mismatch = conflict). Same admission
  constraints as today: session open, idle, `api_kind` pinned, provider
  compatibility validated.
- Engine command `PatchSessionConfig` becomes
  `ReplaceSessionConfig { expected_revision, config }`; the `ConfigChanged`
  event and revision mechanics are unchanged.
- Deleted outright: `FieldPatch<T>`, `SessionConfigPatchInput` and all
  `*Patch` DTOs in `api/src/sessions.rs`, engine `SessionConfigPatch` /
  `OptionalConfigPatch` and every `apply_to`, and the gateway patch
  translation (`core_session_patch_from_api` and helpers).
- **Size check (the "is it small enough" question): yes.** A fully configured
  document is well under 2 KB; even pathological domain allowlists stay in the
  tens of KB. `ConfigChanged` already stores the complete config on every
  change, so full-doc puts add no storage cost — they remove the patch
  vocabulary only.
- **Single document, single endpoint.** Splitting core vs feature configs into
  separately puttable documents would multiply endpoints and revisions without
  benefit: a full-doc put already gives atomic cross-section batch updates,
  and the doc is small enough that "send the whole thing" is never a burden.
  If a granular put is ever needed, a section-scoped variant can be added
  later without changing the model.
- Read path: `SessionView.config` returns the sparse stored document plus
  `configRevision`. The materialized `SessionConfigView`/`ToolConfigView`
  (with their lazy `effective_*` duplication) are deleted; effective reality
  is already visible via `activeTools`/`activeContext`. The
  `SessionConfigChanged` notification keeps carrying the new revision.

### 3. Secure by default (the flip)

Default config = core sections only, `features` empty. That flips today's
defaults: `web_search`/`web_fetch` on → off, `filesystem` edit → off. The
default agent is a model that can process runs and nothing else; profiles are
the primary granting mechanism.

Developer experience: the CLI stops relying on server defaults and sends an
explicit dev config at `session/start` (vfs with edit tools + web + timers), with a
`--bare` flag for the true default. The single source for effective feature →
toolset resolution becomes the gateway materialization (§4); the duplicated
`effective_*` helpers in `api-projection` go away with the materialized view.

### 4. Toolset as derived state; declarative MCP

The installed toolset (`ToolingState`) becomes a pure function of
(features, provider api_kind, linked resources). On config put, the gateway
recomputes the desired toolset from the feature blocks — the existing
`session_toolset_config()` mapping, now keyed off features — diffs against the
current `ToolingState`, and applies an internal engine `ToolPatch`.
`ToolPatch`/`ReplaceTools` and the tools revision stay as internal engine
mechanism; they stop being an external API.

- `features.mcp.servers` declares linked servers by registry id with optional
  per-session overrides (`allowedTools`, `approval`, `deferLoading`) and an
  optional `authGrantId` — a universe-scoped stable identity, so permitted in
  config per §5; grant/policy compatibility is validated at put admission
  against the catalog (today's `auth_ref_for_link` checks) and the token
  broker resolves the grant at request time. This replaces imperative link
  state. `session/mcp/link`/`unlink` either become sugar that
  performs a config put or are dropped (recommend: drop; the CLI/clients edit
  config).
- External `session/tools/update` is removed until a real use case for
  client-registered function tools exists (same "no caller, no surface" logic
  as `session/messages/submit` — its only production users are internal:
  standard toolset provisioning and MCP linking). The engine commands stay.
- Feature-to-tool mapping details (builtin presentation, per-op filesystem
  flags, web-search cache mode) remain internal materialization policy — the
  knob a feature exposes is intent, not tool plumbing. Where a feature grows
  alternative implementations (e.g. a non-OpenAI web search), the block gains
  an optional `impl` selector validated against known implementations; the
  field is reserved, not built, while only one implementation exists.
- Detail to resolve during implementation: `await`/`cancel`/`detach` currently
  ship with the `timer` flag but fleet waiting depends on them — the fleet
  feature must imply the concurrency tools it needs rather than requiring
  `timers`.
- The boundary between config and the remaining imperative APIs is §5.

### 5. Config vs resource APIs: the boundary

Config does not absorb every switch. The system keeps four layers:

1. **Universe catalogs** — what exists: MCP server catalog, environment
   providers/targets, profiles, workspaces. Put-with-revision CRUD (§8).
2. **Session config** — what this session may do and how it behaves:
   capability grants, behavior knobs, references to catalog entries by stable
   id. One revisioned document, idle-only puts.
3. **Session resources** — bindings and instances with their own lifecycles:
   environment attachments, VFS mounts/workspaces, auth grants, bridge
   bindings. Imperative verb APIs, usable mid-session.
4. **Derived state** — toolset, prompt/skill discovery: a pure function of
   (2) + (3), never written directly.

Discriminators for "config or resource verb":

- **Reconstructible from the declaration?** If the end state is a pure
  function of the document plus catalogs — reconciliation synchronous,
  side-effect-free, unable to fail for external reasons — it is config (MCP
  links). If it has its own state machine (provisioning, readiness, failure,
  expiry, moving snapshot heads), it is a resource (environments, workspaces,
  grants). Config puts must never become async provisioning triggers.
- **Mid-session mutability.** Config puts require an idle session; anything
  the agent or runtime must change while running (attach a sandbox, create a
  workspace, refresh a grant, fleet VFS share/isolate) must be a resource
  verb.
- **Acts are never config.** Runs, context append/remove, skill activation,
  OAuth flows are verbs.
- **Sourcing rules are config; the state they act on is not.** Session config
  is the *static* declaration level. `features.vfs.prompts` /
  `features.vfs.skills` declare where instruction and skill material is
  sourced from within the VFS (roots defaulting to the conventional
  `/prompts`, `/skills` — today hardcoded in the gateway as
  `conventional_vfs_prompt_root_specs`); they live under `vfs` because
  sourcing is storage-specific — environment-based sourcing will ride
  environment bindings later. Which mounts exist is VFS resource state with
  its own effects (context injection); what actually gets discovered and
  injected is derivation over sourcing rules × mounted state. The same split
  holds for environments: the grant and allowed targets are config, the
  attachment and its runtime effects are state.

Relationship rules (these prevent re-creating the config/toolset two-writer
problem at the next layer up):

- Resource verbs are **authorized by** config — `environments/attach` requires
  `features.environments`, a mount put requires `features.vfs` — but
  never modify config.
- Config may reference only universe-scoped identities (registry ids, profile
  ids), never session-runtime instance ids, so puts validate against catalogs
  at admission and never race a resource lifecycle.
- Derivation reads both layers: process/job tools materialize when
  `features.environments` is granted **and** an environment binding is
  active.
- Revoking a feature while dependent bindings are live is a **validation
  error** that lists the bindings; teardown is explicit, a config put never
  closes resources as a side effect.

Borderline case, decided: VFS mount declarations (path + source + access)
look declarative and profiles template them, but mounts are created
mid-session (agent-created workspaces, fleet share/isolate), so they stay
resource verbs. If they stabilize into pure declarations they can lift into
`features.vfs.mounts` later without changing the model.

Refinement (2026-07-09, after slice 6): two independent tests decide
config membership, and each excluded candidate fails a different one.

- **Stability**: a config declaration must be satisfiable synchronously at
  admission (pure derivation from catalogs) and stay satisfied without the
  external world's cooperation; failure must be call-scoped, never
  binding-scoped. Declaring something that can independently fail (a VM, a
  bridge) turns "applied" into an aspiration and implies a standing
  reconciler with "declared but degraded" as a config state — a different
  product decision we deliberately did not make. Environments fail this
  test; MCP links pass (a down server fails tool calls, not the
  declaration). Known bounded edge: deleting a catalog server that live
  session configs reference makes those sessions' next config put fail
  resolution until the dangling link is removed — fail-loud at put time, by
  design.
- **Ownership**: config is the operator's standing declaration, changed
  rarely at idle; work state accumulated by the agent/runtime must not live
  in it, or profile apply ("make the session look like the profile")
  becomes destructive to work and every runtime change churns the config
  revision. VFS mounts fail this test even though they pass stability (CAS
  is internal and durable).
- Profile `mounts`/`environments` are **one-shot setup steps** (applied
  once, best-effort, counted in the apply summary), not standing
  declarations — which is why environments may appear in a profile without
  contradicting their exclusion from session config.

### 6. Feature versioning

Each feature block has a `version: u32`. Rules:

- Omitted on put/start → admission stamps the deployment's current default
  version for that feature into the stored doc (pinning), so later default
  bumps never change a running session's behavior — important for long-lived
  always-on sessions (rooms).
- Validation rejects versions the deployment doesn't support. Changing a
  version is an ordinary config put (idle-session rule applies).
- Breaking feature changes ship as `version: n+1` with the old materialization
  kept until retired. Core sections don't carry versions; they evolve via the
  event schema version (`CORE_AGENT_SCHEMA_VERSION`) as today.

### 7. Profiles

A profile's `config` field becomes the same sparse document. Composition
semantics get simpler and deterministic:

- **At start:** explicit start config and profile config merge per top-level
  section as today (explicit wins whole-section), then flow through put
  validation.
- **`session/profiles/apply`:** the profile's config document **replaces** the
  session config via an internal `session/config/put` (guarded by
  `expected_config_revision` as today). No more input→patch conversion
  (`session_config_patch_from_input`, `gateway/service/profiles.rs:530-558`).
  Sections absent from the profile revert to defaults — apply means "make the
  session look like the profile", not "overlay".
- `profiles/update` is deleted; `profiles/put` (already full-doc +
  expected-revision) is the only write. `AgentProfileUpdatePatch` goes away.

### 8. Registry surfaces converge on put

- **MCP catalog:** add `revision: u64` to server records; replace
  `mcp/servers/create` + `mcp/servers/update` with **`mcp/servers/put`**
  (create-or-replace, `expected_revision` checked when the record exists —
  exact `profiles/put` semantics). Fixes the current last-write-wins gap.
  `McpServerUpdatePatch` and the `Option<Option<T>>` store update record go
  away.
- **Kept as-is:** `vfs/workspaces/update` (already put + revision),
  `session/context/append` (keyed idempotent upsert is the right semantic),
  `environments/providers/register` (registration upsert; revision can be
  added later if concurrent editing appears), `auth/grants/import`
  (insert-or-ignore; noted gap: grants are not re-importable — out of scope).
- With this, `FieldPatch<T>` has zero remaining users and is deleted from
  `crates/api`, along with all `FieldPatchOf*` types in the generated TS
  client.

### 9. Remove the `session/messages/submit` RPC surface

Traced 2026-07-09: the RPC method has **zero production callers** — the CLI
submits via `session/runs/start`, the TS wrapper `submitMessage` is never
invoked, and only unit/schema tests reference the route. Its one documented
future consumer is the unbuilt ACP/A2A adapter idea
(`docs/roadmap/later/pNNN-acp-a2a-protocol-adapters.md`). Per the
capability-minimal posture: no use case, no surface.

- Remove: method constant + dispatch (`api/src/constants.rs:32`,
  `rpc.rs:296`), `submit_message` on the service trait (`service.rs:110-113`),
  `MessageSubmitParams`/`MessageSubmitResponse` (`runs.rs:56-74`), the gateway
  handler (`gateway/service/mod.rs:2126-2193`), the TS `submitMessage`
  wrapper, and regenerate contract artifacts.
- **Keep** the engine `SubmitMessage` command and mailbox machinery — they are
  load-bearing for fleet `agent_send` (via `deliver_message_for_fleet`,
  `gateway/service/mod.rs:1042`) and detached-promise follow-ups
  (`temporal-workflow/src/workflow/drive.rs:210-256`). The ACP/A2A adapter can
  reintroduce an external surface over the same command when it becomes real.

## Decisions

| Decision | Recommendation | Alternative considered |
|---|---|---|
| Update semantics | Full-doc put + `expected_revision` everywhere | Keep field patches (rejected: two vocabularies, 3× type surface) |
| Section name | `features` (some blocks carry behavior tuning beyond pure authority; "capability" has a narrower security-literature meaning) | `capabilities` — fine if the grant framing should dominate |
| Doc granularity | One document, one put endpoint | Split core/feature docs (rejected: endpoint + revision proliferation, no atomicity win; revisit only for a different-principal policy overlay) |
| `features` representation | Struct-of-options (wire-map-shaped, exhaustive match, unknown keys rejected) | `Map<FeatureId, Value>` (rejected: open-world extensibility this product doesn't want; typing loss) |
| Storage shape | Sparse doc stored; admission materializes only `model` + feature versions | Fully materialized storage (rejected: poor round-tripping, default changes need rewrites) |
| Defaults | Absence = default; features default **off** (flips web/filesystem) | Keep default-on web/fs (rejected: contradicts secure-by-default) |
| `expected_revision` | Optional, `None` = unconditional (uniform with `profiles/put`) | Required (rejected: breaks create/scripting flows, inconsistent) |
| `session/tools/update` | Remove external method; toolset is derived | Keep Replace-only variant (fallback if a client-tool use case appears) |
| `session/mcp/link`/`unlink` | Drop in favor of `features.mcp` config edits | Keep as sugar over config put |
| VFS mounts | Stay resource verbs (created mid-session by agent/fleet); may lift into `features.vfs.mounts` if they stabilize | Declare in config now (rejected: fails the mid-session-mutability test) |
| Prompts/skills sourcing | Sub-blocks of `features.vfs` (sourcing is storage-specific; environment sourcing rides bindings later); discovery stays derived, activation stays a verb | Top-level `prompts`/`skills` features (rejected: cross-feature dependency on the filesystem grant) |
| `session/messages/submit` | Remove RPC surface, keep engine command | Keep dormant for ACP/A2A (rejected: dead surface, easy to re-add) |

## Implementation slices

Engine config core first; everything downstream is allowed to break while the
structures are iterated. One alignment slice then fixes the workspace and cuts
the wire surface over. No shims.

1. **Engine config core** — new `SessionConfig` sections
   (`model`/`generation`/`limits`/`context`/`features`), delete engine patch
   types (`SessionConfigPatch`, `OptionalConfigPatch`, all `apply_to`),
   `PatchSessionConfig` → `ReplaceSessionConfig`, version pinning + validation
   updates (`engine/src/core/components/{config,command,lifecycle}.rs`,
   `core/admit.rs`); regenerate engine fixtures. Downstream crates may not
   compile during this slice — `cargo test -p engine` is the green bar.
2. **Workspace alignment + API cutover** — fix everything against the new
   structures in one pass: `default_session_config`
   (`temporal-workflow/src/config.rs`); gateway translation + feature→toolset
   materialization (`gateway/service/{mod,api_config}.rs`); reasoning-effort
   elaboration moved to `llm-runtime` (`llm-runtime/src/params.rs`); new
   config DTOs (`deny_unknown_fields`), `session/config/put`, delete
   `session/update` + patch DTOs + `FieldPatch`, `SessionView.config` =
   sparse doc, delete materialized config views
   (`api/src/{sessions,views,constants,rpc,service}.rs`, `api-projection`);
   **default flip to secure-by-default**; CLI dev config + `--bare`
   (`crates/cli`); `cargo run -p api --bin export-schema`; regenerate TS
   client (`interop/`).
3. **Declarative MCP + derived tools** — `features.mcp`, reconcile
   `ToolingState` on put, remove external `session/tools/update` and
   `session/mcp/link`/`unlink` (`gateway/service/{mcp_api,tools_api}.rs`).
4. **Registry puts** — MCP record `revision`, `mcp/servers/put` replacing
   create/update; delete `profiles/update`
   (`api/src/{mcp,profiles}.rs`, `store-pg/src/{mcp,profile}.rs`,
   `crates/mcp`, `crates/profiles`).
5. **Remove `session/messages/submit`** RPC surface (see §9).
6. **Profiles + docs** — profile apply = internal config put; update
   `README.md`, `AGENTS.md`, `docs/design.md`, this file's status.

## Open questions

- Does any near-term client need runtime-registered function tools (would
  resurrect a `session/tools/put`)? Current answer: no known caller.
- Should `features.environments` gate only attach/activate or also the
  process/job toolset independently of `workspace`? (Today process tools ride
  the builtin toolset next to fs tools.)
- Exact default version bump policy: per-feature constant in deployment
  config vs compiled-in table (leaning compiled-in until multi-version
  actually exists).
