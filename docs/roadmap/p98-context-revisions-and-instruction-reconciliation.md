# P98: Context Revisions and Effective Instruction Reconciliation

**Status:** Implemented 2026-07-10.

Implemented: optional CAS on all three direct context edit commands, structured
revision-conflict rejections, generic admission correlation replacing
`context_key`, removal of the `instructions_ref` bootstrap override, source-map
prompt publication, and guarded effective instruction reconciliation for
profile and VFS updates. Conflicts are returned to the outer operation without
an automatic retry loop.

## Goal

Add optional compare-and-set semantics to direct context edits, replace the
context-key-specific workflow failure shim with generic admission correlation,
and make managed profile/VFS instruction updates publish one inspectable
effective instruction set.

The intended behavior is deliberately scoped:

1. Every active `ContextEntryKind::Instructions` entry is included in model
   requests in context-key order. There is no planner-only fallback or
   suppression rule.
2. After bootstrap or a managed profile/VFS instruction reconciliation,
   `instructions.000.default` is active exactly when no non-default instruction
   entry is active.
3. Concurrent managed instruction updates cannot silently overwrite one
   another. A stale update receives a structured conflict; P98 does not hide
   that conflict behind an automatic retry loop.
4. Direct internal context edits remain direct. They may intentionally create
   an instruction state that does not follow the managed fallback policy.
5. Expected context revisions remain optional. Omitting one preserves the
   current unconditional command behavior and existing no-op/error semantics.

## Problem

Today the workflow bootstraps `instructions.000.default`, profile application
upserts `instructions.050.profile`, and VFS prompt refresh replaces
`instructions.100.prompts.*`. The provider adapters concatenate all active
instruction entries, so a profile or workspace prompt augments the product
default instead of replacing it.

Suppressing the default only while planning would make
`SessionView.activeContext` disagree with the provider request. Fallback
selection therefore belongs in the managed active-context update, while the
planner and provider adapters keep the simple rule that every active
instruction is model input.

Building that managed update requires a read-modify-write over the instruction
namespace. The direct context commands have no expected-revision guard, so two
concurrent source updates can both read the same state and the later full-prefix
replacement can erase the earlier update.

The workflow admission envelope also carries an optional `context_key` solely
to associate asynchronous command failures with entries in the public
multi-entry context APIs. This leaks one command's business key into generic
workflow transport and still cannot distinguish concurrent attempts involving
the same key. P98 replaces it with generic per-attempt correlation.

## Design

### 1. Optional revisions on direct context edits

Add `expected_revision: Option<u64>` to the three caller-computed context
mutation commands:

```rust
CoreAgentCommand::UpsertContext {
    expected_revision: Option<u64>,
    key: ContextEntryKey,
    entry: ContextEntryInput,
}

CoreAgentCommand::ReplaceContextPrefix {
    expected_revision: Option<u64>,
    key_prefix: ContextEntryKey,
    entries: BTreeMap<ContextEntryKey, ContextEntryInput>,
}

CoreAgentCommand::RemoveContext {
    expected_revision: Option<u64>,
    key: ContextEntryKey,
}
```

Admission validates a supplied revision against `state.context.revision`
before no-op detection or mutation. A mismatch returns
`CommandRejectionKind::RevisionConflict` with structured details containing the
expected and actual context revisions. `None` leaves the command unconditional.

The workflow preserves the engine `CommandRejection` on its admission failure
instead of flattening the only machine-readable result into a string. Ordinary
display messages remain for logs and API errors; callers branch on the
structured rejection.

All existing call sites pass `expected_revision: None` unless they have built a
replacement from a loaded context revision. Input preprocessing must carry the
field through when it rebuilds an `UpsertContext` command.

`CompactContext`, run input, steering, tool results, and other operational
commands do not gain an expected revision. They derive their context events
from workflow state at admission time rather than from a caller-built context
document.

The public `session/context/append` and `session/context/remove` methods remain
unconditional. They are multi-command APIs without one atomic revision
boundary. API-level context CAS would require a batch command and is separate
work.

### 2. Generic admission correlation

Replace `AgentAdmission.context_key` and
`AgentAdmissionFailure.context_key` with an optional generic correlation token:

```rust
pub struct AgentAdmission {
    pub command: CoreAgentCommand,
    pub correlation_token: Option<String>,
}

pub struct AgentAdmissionFailure {
    pub correlation_token: Option<String>,
    pub submission_id: Option<SubmissionId>,
    pub kind: AgentAdmissionFailureKind,
    pub message: String,
    pub rejection: Option<CommandRejection>,
}
```

A correlation token identifies one attempt to admit one command. The workflow
copies it to any preprocessing or command-admission failure. It is transport
metadata only: it is not part of `CoreAgentCommand`, is not written to engine
events, and is not exposed as a public API identifier.

For `session/context/append` and `session/context/remove`, the gateway generates
one token per submitted command and retains a request-local mapping from token
to context key while it waits. This replaces the context-specific field without
changing per-key API results. A managed full-prefix replacement similarly uses
one token so its caller observes only its own rejection.

`submission_id` remains distinct. It is a client-visible, durable idempotency
key for a logical run or message and is stored in engine history. Several
admission attempts may legitimately carry the same run `submission_id`; each
attempt can have its own ephemeral correlation token.

P98 does not add an admission outcome ledger. Success is observed from the
expected durable state, as today; the token correlates asynchronous failures.

### 3. Prompt assembly stays outside the engine

VFS prompt discovery and assembly remain in `tools::prompts`. They perform VFS
reads, blob writes, report generation, and source ordering, none of which
belongs in the deterministic engine.

The assembler stops deciding whether to emit a `CoreAgentCommand`. Instead it
always returns the complete desired map for the VFS-owned prompt source,
including an empty map. The gateway then compares and reconciles the complete
effective instruction namespace. This distinction matters when the VFS subset
is unchanged but the default/profile relationship still needs reconciliation.

The engine remains unaware of product defaults, profiles, VFS prompts, and
instruction-source precedence. It provides only generic validation, optional
context CAS, and atomic prefix replacement.

### 4. One managed instruction reconciler

The gateway owns managed instruction-source policy. A helper receives a loaded
session state and one complete source update, then constructs the desired map
under the segment prefix `instructions`:

1. Copy active `instructions.*` entries into a
   `BTreeMap<ContextEntryKey, ContextEntryInput>`.
2. Replace the subset owned by the source being updated.
3. Remove `instructions.000.default` from the candidate.
4. If no non-default instruction remains, insert the product default.
5. If the complete candidate equals the active instruction map, return an
   unchanged result without submitting a command.
6. Otherwise submit one `ReplaceContextPrefix` for `instructions` with
   `expected_revision` set to the loaded context revision and a fresh
   correlation token.
7. Wait for the desired map or the failure carrying that token. Return a typed
   conflict on `RevisionConflict`; do not retry inside the reconciler.

The full-prefix replacement produces one `KeyPrefixReplaced` event, so readers
never observe half of a managed update. A changed source rematerializes the
retained instruction entries in key order; an identical complete map remains a
no-op and does not bump the context revision.

The operation that encountered a conflict may be repeated at its normal outer
boundary. Profile application and run-start refresh are already idempotent, so
an explicit repeat reloads the new state, rebuilds the candidate, and converges
without a hidden retry limit or backoff policy.

### 5. Managed source ownership and scope

Managed instruction sources are:

| Source | Owned key or prefix | Update |
| --- | --- | --- |
| Product fallback | `instructions.000.default` | Bootstrap or reconciler |
| Profile | `instructions.050.profile` | Zero or one entry |
| Mounted VFS prompts | `instructions.100.prompts` | Complete source set |

Profile application always supplies a complete profile source update. A profile
with no `instructions` removes a previous `instructions.050.profile` entry.
`Text` is stored as a blob first; `TextRef` validates and reuses its blob.

VFS prompt refresh always supplies the complete assembled prompt map. An empty
map clears stale VFS prompt entries. Profile and VFS instructions remain
additive to each other; only the product fallback is mutually exclusive with
non-default instructions.

The reconciler runs only for:

- profile instruction apply or clear;
- VFS prompt refresh before an idle run;
- VFS prompt refresh after profile mounts have been applied;
- session start with a profile, through the same profile-application path.

Bootstrap continues to insert the product default directly. Arbitrary internal
`UpsertContext`, `ReplaceContextPrefix`, and `RemoveContext` calls do not invoke
the reconciler and are not constrained by its fallback policy. A later managed
source update starts from whatever active instruction entries exist at that
time, preserves entries outside its owned subset, and reapplies the managed
fallback rule.

### 6. One product default

Remove the unused deployment override path:

- `AgentSessionArgs.instructions_ref`;
- `GatewayAgentApiBuilder.instructions_ref`;
- `GatewayAgentApi.instructions_ref`;
- `GatewayAgentApiBuilder::with_instructions_ref`.

Normal hosted construction never sets the override; its only repository use is
a live test. Bootstrap always writes `default_instructions()` to CAS and
activates the resulting reference. The reconciler writes the same bytes when it
must restore the fallback, producing the same content-addressed reference. The
live test that needs custom instructions uses an inline profile instead.

This keeps the product fallback unambiguous and avoids adding session metadata
solely to remember which optional bootstrap override was used.

### 7. Refresh timing

Prompt refresh runs while the session is idle immediately before a new run.
Profile application refreshes prompts after applying profile mounts and before
returning its final session view. A successful `session/start` or
`session/profiles/apply` response therefore exposes the effective managed
instruction context for the resources installed by that operation.

VFS instruction files may change between runs. The next run-start refresh
assembles and reconciles them before run admission, so the first provider
request sees the new set. P98 does not refresh instructions between turns or
rewrite an in-flight provider request.

The planner and provider adapters add no fallback logic. They continue to place
all active instruction entries first in context-key order. OpenAI joins their
text into top-level `instructions`; Anthropic joins it into `system` content.

## Conflict behavior

P98 uses rejection rather than automatic retry:

1. Two managed operations may load context revision `r`.
2. The first guarded replacement succeeds and advances the context revision.
3. The second replacement is rejected with expected `r` and the new actual
   revision.
4. The second outer operation returns a typed conflict without changing
   instruction state.
5. Repeating that outer operation reloads the first update, preserves it while
   rebuilding the complete map, and can then succeed.

This prevents silent lost updates while keeping retry ownership visible to the
caller. P98 can add an internal retry later without changing the engine CAS or
correlation model.

## Implementation

1. **Completed.** Add optional expected revisions and a shared validator to the
   three direct context commands in `engine`. Add structured revision-conflict
   rejection details and admission tests.
2. **Completed.** Update command constructors with `expected_revision: None`,
   and preserve the supplied value through workflow input preprocessing.
3. **Completed.** Replace admission/failure `context_key` with
   `correlation_token`; preserve structured engine rejections on
   `AgentAdmissionFailure`; update public context append/remove waits to use
   request-local token-to-key maps.
4. **Completed.** Remove the `instructions_ref` workflow/gateway override and
   convert its live test to an inline profile.
5. **Completed.** Refactor prompt publication to return the complete desired
   VFS instruction map instead of an optional engine command. Update the
   in-process test runner to use the same effective-map policy for prompt
   refresh.
6. **Completed.** Add the gateway managed instruction reconciler, including
   complete-map no-op detection, guarded full-prefix submission, exact failure
   correlation, and typed conflict propagation.
7. **Completed.** Route profile instruction apply/clear and VFS prompt refresh
   through the reconciler. Refresh VFS prompts after profile mounts are
   applied.
8. **Completed.** Keep bootstrap insertion, engine planning, and provider
   materialization otherwise unchanged.

P98 changes internal engine/workflow payloads but no JSON-RPC method shapes, so
the committed API contract artifacts do not need regeneration.

## Tests

- A stale expected context revision rejects `UpsertContext`,
  `ReplaceContextPrefix`, and `RemoveContext` with structured expected/actual
  values; `None` preserves each command's existing behavior.
- Input preprocessing preserves an `UpsertContext` expected revision.
- Admission failures echo the submitting correlation token; concurrent
  same-key attempts cannot claim one another's failures.
- Public context append/remove still produce per-key outcomes after removing
  `AgentAdmission.context_key`.
- Default instructions are the only active instructions in a bare session.
- A profile instruction atomically replaces the default.
- VFS instructions atomically replace the default.
- Profile plus VFS instructions keeps both custom sources and no default.
- Clearing one managed custom source preserves the other and does not restore
  the default.
- Clearing the final managed custom source restores the default.
- Applying a profile without instructions clears a previous profile entry.
- Prompt assembly returns a complete desired source map even when it is empty
  or equal to the currently active VFS subset.
- Concurrent profile and VFS replacements from one revision produce one
  success and one typed conflict, never a silent overwrite; explicitly
  repeating the losing outer operation converges to both sources.
- Profile mounts are installed before their VFS prompt source is assembled and
  reconciled.
- The key-sorted active instruction set matches the instruction refs and text
  materialized by the OpenAI and Anthropic adapters.
- The custom-instruction live test uses a profile and no longer depends on a
  workflow bootstrap override.

## Non-goals

- No automatic retry, retry limit, or backoff policy.
- No global engine enforcement of the managed fallback relationship.
- No automatic reconciliation after arbitrary direct context edits.
- No engine command specialized for product/profile/VFS instructions.
- No planner-side suppression of visible context entries.
- No required expected revisions.
- No admission success ledger or generic command idempotency model.
- No API-level context CAS for the existing multi-entry append/remove methods.
- No context patch document or field-level patch vocabulary.
- No change to prompt file conventions, source ordering, or provider-specific
  instruction formatting.
