# P91: CoreAgent Structure Cleanup — Closed Event Model And Core FSM

**Status**
- Proposed 2026-07-03.
- Slice 1 completed 2026-07-03: open kernel deleted (`AgentDomain`,
  codec traits, generic session workflow helpers, `replay()`, compat shim),
  `admit_command` / `apply_event` / `plan_next` are plain functions,
  `CoreAgentDrive` carries only session id + state + head, `AgentAdmission`
  carries `CoreAgentCommand` directly (the `InvalidCommand` admission failure
  kind became unrepresentable and was removed), and decode guards the
  `lightspeed.core.` prefix + version instead of a 30-entry kind allowlist.
- Slice 2 completed 2026-07-03: `CoreAgentEvent` wrapper struct deleted — the
  enum (formerly `CoreAgentEventKind`) is now the event. `entry.event.kind` →
  `entry.event`, `CoreAgentEventProposal.kind` → `.event`, and stored envelope
  payloads lost one nesting level (`{"kind":{"lifecycle":"closed"}}` →
  `{"lifecycle":"closed"}`); engine fixtures regenerated. Existing persisted
  logs with the old payload shape are invalidated (greenfield, no migration).
  Slices 3–4 pending.
- Engine-focused refactor with downstream fixups in `temporal-workflow`,
  `temporal-server`, `api-projection`, `test-support`, `store-fs`, `store-pg`,
  and `eval`.
- Supersedes the SDK-era "open kernel" posture from P54 (composable agent
  kernel). Lightspeed is a product; CoreAgent is the only agent domain, now and
  going forward. The command/event vocabulary and the core FSM are **closed**.
- **Greenfield: breaking changes are fine.** No migrations, no compat shims, no
  deprecated aliases, no dual-format decode paths. Stored payload shapes and
  store schemas change in place; local stacks are reset.

## Goal

Commit the engine to a fixed event vocabulary and a closed core FSM:

1. Delete the extensibility layer that existed so third parties could plug in
   their own commands, events, state, and logic.
2. Collapse single-implementation traits into plain functions.
3. Remove concepts that only made sense with multiple agent domains
   (`AgentHandle` on session records, the `DynamicCommand` envelope).
4. Keep the durable event envelope — it is the log's storage format, not an
   extensibility feature — and rename it so the code says so.

## Inventory

Every open abstraction in `engine` was traced to its consumers across the
workspace (2026-07-03, branch `multi-tenant`):

| # | Structure | Location | Consumers found | Action |
|---|-----------|----------|-----------------|--------|
| 1 | `AgentDomain` trait + `CoreAgentDomain` | `session/domain.rs`, `core/domain.rs` | One impl, zero constructions anywhere | Delete |
| 2 | Generic session workflow helpers (`append_admitted_command`, `append_event_proposals`, `SessionWorkflowError`, `AppendAppliedEvents`, `encode_uncommitted_event`, `decode_session_entry`) | `session/workflow.rs` | Own tests + `lib.rs` re-exports only | Delete file |
| 3 | `replay()` | `session/replay.rs` | Zero callers (`temporal-workflow/rehydrate.rs` has its own loop) | Delete file |
| 4 | Compat shim re-exports | `core/workflow.rs` | Zero importers via that path | Delete file |
| 5 | `AdmitCommand` / `ApplyEvent` / `PlanNext` traits + `CoreAdmitCommand` / `CoreApplyEvent` unit structs | `core/transition.rs`, `core/admit.rs`, `core/apply.rs` | One impl each; downstream crates import the trait just to call a method on a stateless unit struct | Collapse to functions |
| 6 | `CorePlanner` with `Vec<Box<dyn PlanNext>>` layers + four planner unit structs | `core/planning.rs`, components | Only the fixed `core()` composition is ever built | Collapse to a function |
| 7 | `CommandCodec` / `EventCodec` / `JoinsCodec` traits | `session/codec.rs` | One implementor (`CoreAgentCodec`); the `JoinsCodec` impl already delegates to same-named inherent methods | Delete traits, make methods inherent |
| 8 | `DynamicCommand` envelope | `session/dynamic.rs` | One kind ever (`lightspeed.core.command` v1); encoded in the gateway, decoded immediately in workflow admission | Delete; pass `CoreAgentCommand` directly |
| 9 | 30-entry decode kind allowlist | `core/codec.rs` | Hand-synced with the encoder's match; decode parses the payload wholesale (payload embeds its own discriminant), so the list is only a sanity guard | Replace with prefix + version check |
| 10 | `CoreAgentEvent { kind: CoreAgentEventKind }` wrapper | `core/components/event.rs` | Single-field struct; forces `entry.event.kind` everywhere and nests stored payloads one level | Flatten |
| 11 | `AgentHandle` on session records | `session/ids.rs`, `storage/session.rs`, both stores | Every production caller hardcodes a constant (`lightspeed.agent`, `lightspeed.default`); `ListAgentSessions` has zero callers outside the stores | Delete the concept |
| 12 | `Dynamic*` naming (`DynamicEvent`, `DynamicSessionEntry`, `DynamicUncommittedSessionEvent`, `DynamicJoins`) | `session/dynamic.rs`, `session/log.rs` | Load-bearing storage format (store-fs, store-pg, activities, projection) | Keep, rename to `Stored*` |

## What stays (earns its structure)

- **The stored event envelope** (`kind` string + `version` + JSON payload +
  flat string joins). It is the durable, language-neutral log format used by
  `store-fs`, `store-pg`, Temporal activities, and `api-projection`. It stays —
  reframed as the log format of the closed CoreAgent model, not a plug-in
  surface.
- **`CoreAgentLlm` / `CoreAgentTools`** (`core/io.rs`) — genuine substrate
  variation: test-support fakes, `llm-runtime`, Temporal activities.
- **`CoreAgentDrive` / `CoreAgentAction`** — the substrate-neutral action
  machine *is* the product FSM; everything else collapses toward it.
- **Generic `SessionEntry<E, J>`** — cheap, two instantiations
  (`CoreAgentEntry`, stored entries). Not worth splitting into duplicate
  concrete structs.

## End State

- `engine::session` — session-log primitives only: ids, `SessionEntry` /
  `UncommittedSessionEvent`, the stored event envelope, `CodecError`. No domain
  trait, no codec traits, no replay/workflow helpers. Module docs stop claiming
  extensibility.
- `engine::core` — *the* FSM, not "one pluggable domain": the closed
  command/event/state vocabulary, free functions `admit_command(state,
  command)`, `apply_event(state, entry)`, and `plan_next(state)`,
  `CoreAgentCodec` with inherent methods, the drive machine.
- Substrates call the functions directly; no trait imports anywhere downstream.
- `AgentAdmission` carries `CoreAgentCommand` directly; serde at the signal
  boundary is the validation.
- Session records identify sessions, not agent domains: no `agent_handle`
  field, column, or id type.

Estimated net deletion: ~600–800 lines plus five traits, six unit structs, and
one stored-record concept.

## Implementation Slices

Each slice is a separately reviewable diff. Slice 1 changes no stored bytes;
slices 2 and 3 each change exactly one persisted shape.

### Slice 1 — Delete the open kernel (no stored-format changes)

1. Delete `session/domain.rs`, `session/replay.rs`, `session/workflow.rs`,
   `core/domain.rs`, `core/workflow.rs`.
2. `session/codec.rs`: delete `CommandCodec` / `EventCodec` / `JoinsCodec`;
   keep `CodecError`.
3. `core/codec.rs`: make all encode/decode methods inherent on
   `CoreAgentCodec`; delete `CoreAgentDomainError`'s home (`core/domain.rs`)
   and the command encode/decode once item 6 lands. Replace
   `is_core_agent_event_envelope_kind`'s 30-entry allowlist with a
   `lightspeed.core.` prefix + `version == 1` check.
4. `core/transition.rs`: delete the three traits; keep
   `CoreAgentEventProposal`.
5. `core/admit.rs`: `CoreAdmitCommand` → `pub fn admit_command(state, command)
   -> Result<Vec<CoreAgentEventProposal>, CommandError>`.
6. `core/apply.rs`: `CoreApplyEvent` → `pub fn apply_event(state, entry) ->
   Result<(), DomainError>`.
7. `core/planning.rs`: `CorePlanner` → `pub fn plan_next(state)` trying run →
   tool → context → turn in order; the four component planner unit structs
   become module functions (`run::plan_next`, etc.).
8. Delete `DynamicCommand`: `AgentAdmission.command` becomes
   `CoreAgentCommand`; the gateway passes it through; workflow admission drops
   its decode step. Delete the command fixture + its test.
9. `CoreAgentDrive`: drop the `codec` / `admit` / `apply` / `planner` fields
   (all stateless); call the functions.
10. Fix downstream imports and call sites: `temporal-workflow` (`rehydrate`,
    `workflow/{mod,drive,admissions}`, `types`, tests), `temporal-server`
    (gateway service, fleet, activity tests, `fake_loop`), `api-projection`,
    `test-support`. Prune `lib.rs` / `session/mod.rs` / `core/mod.rs`
    re-exports; update the P54-era module docs.

### Slice 2 — Flatten `CoreAgentEvent` (stored payload shape changes)

1. Delete the wrapper struct; rename `CoreAgentEventKind` → `CoreAgentEvent`
   (the enum *is* the event).
2. `entry.event.kind` → `entry.event`; `CoreAgentEventProposal.kind` →
   `.event`.
3. Stored envelope payloads lose one nesting level:
   `{"kind":{"lifecycle":"closed"}}` → `{"lifecycle":"closed"}`. Regenerate the
   engine fixtures under `crates/engine/fixtures/`.

### Slice 3 — Drop `AgentHandle` (schema change)

1. Remove `agent_handle` from `SessionRecord`, `CreateSession`,
   `CreateClonedSession`, `CreateForkedSession`; delete `ListAgentSessions`
   and its store methods (zero callers outside the stores).
2. `store-pg`: edit `migrations/001_core.sql` in place — drop the column, the
   `sessions_agent_handle_format` check, and the
   `sessions_agent_handle_session_id_idx` index; fix inserts/selects. Reset
   local stacks via `local/` helpers.
3. `store-fs`: drop the field from the session record JSON.
4. Delete the `AgentHandle` string id from `session/ids.rs`.
5. Fix constructors across `temporal-server` (activities, fleet),
   `test-support`, `eval`, `store-*` tests, and `llm-runtime` live tests.

### Slice 4 — Rename the envelope types (mechanical)

1. `DynamicEvent` → `StoredEvent`, `DynamicSessionEntry` →
   `StoredSessionEntry`, `DynamicUncommittedSessionEvent` →
   `UncommittedStoredEvent`, `DynamicJoins` → `StoredJoins`;
   `session/dynamic.rs` → `session/stored.rs`.
2. Stored bytes are unchanged (type names don't serialize); this is purely a
   naming pass so the code stops implying pluggability. Update the `store-fs`
   module docs that reference the old names.

## Verification

- `cargo build` + full `cargo test` workspace-wide after every slice.
- `api` wire types are untouched (the cleanup is engine/storage-internal), so
  no `interop/contract` regeneration is expected; `cargo test -p api` confirms.
- Slice 2: fixture round-trip tests in `engine` prove the new payload shape.
- Slice 3: `store-pg` live tests against a reset local stack.
