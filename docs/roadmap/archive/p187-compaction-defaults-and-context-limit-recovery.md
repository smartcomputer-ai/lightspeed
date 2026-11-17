# P187 — Compaction defaults and context-limit recovery

**Status:** Implemented, 2026-10-01. Final validation results are recorded below.

Builds on [provider-native compaction](archive/p64-provider-native-compaction.md)
and [provider-safe context repair](p186-provider-safe-media-and-context-entry-redaction.md).
Supersedes the former proposal's opt-in defaults, manual-only standalone
behavior when its threshold is absent, and deferral of automatic fallback.

## Outcome

Long-running sessions use engine-managed standalone compaction by default,
with native operations for supported Responses and Anthropic Messages routes
and Lightspeed-managed summarization for Chat Completions providers, including
OpenAI and DeepSeek. One deterministic scheduling and recovery lifecycle
serves every route; adapters preserve each provider's native continuation rules.

Provider-triggered compaction remains a fully supported, first-class explicit
mode, especially for OpenAI Responses where provider-managed triggering is a
preferred operating choice. It runs inside ordinary generations and keeps its
native capture, pruning, usage, and continuation behavior. Engine-managed
standalone defaults do not replace or weaken this path.

An enabled session can recover from a context-length failure even when no
threshold was supplied. Recovery preserves recent work, compacts older material
through bounded requests, and resumes the same run without repeating completed
tool side effects. Explicitly disabling compaction disables both proactive
compaction and automatic context-length recovery.

An explicit `session/context/compact` call remains available in every mode,
including Disabled. It selects the native standalone operation when supported,
otherwise the Lightspeed summarizer. It does not change the configured policy.
Compaction changes active context through ordinary events; durable history and
original content remain subject to their existing retention policies.

## Baseline implementation and verification

Before this change, the repository provided:

- `ContextConfig.compaction: Option<CompactionPolicy>` with Disabled,
  ProviderTriggered, and ProviderStandalone modes. Omission currently disables
  compaction for every route; the UI labels that omission Engine default.
- Provider-triggered compaction for OpenAI Responses and Anthropic Messages,
  including optional thresholds, native request lowering, exact native block
  retention, and deterministic pruning. Chat Completions rejects this mode.
- OpenAI Responses standalone `/responses/compact` through the runtime,
  in-process runner, and hosted worker.
- Anthropic Messages and Chat Completions standalone compaction through ordinary
  summary generations. Anthropic's new native on-demand operation is not wired.
- Manual compaction admission restricted to ProviderStandalone while no run is
  active or queued. Automatic standalone compaction requires an explicit
  threshold and runs only at that idle boundary.
- A standalone snapshot covering all eligible conversation entries, with no
  protected recent tail. Instructions and current catalogs survive separately.
- Typed provider context-length classification in `llm-clients`, but the generic
  executor maps it to an ordinary request rejection carrying the message.
  In-band context-limit finishes have a separate path, without the recovery
  sequence proposed here.
- OpenAI standalone result handling that retains compaction items but filters
  other returned items. This needs correcting to preserve the complete native
  compacted window.

Live verification performed during the preceding implementation work:

| Route | Verified behavior |
| --- | --- |
| OpenAI Responses | Provider-triggered capture, engine pruning, continuation, manual standalone, and threshold standalone |
| Anthropic Messages, Sonnet 5 and Opus 5.5 | Provider-triggered capture, engine pruning, and continuation |
| Anthropic Messages, Opus 5.5 | Manual and threshold standalone through the existing summary-generation path |
| OpenAI Chat Completions, GPT-5.5 | Standalone summary facts and continuation |
| DeepSeek Chat Completions, V4 Pro | Standalone summary facts and continuation using the selected endpoint and credentials |

These checks establish the existing paths, not native Anthropic on-demand
compatibility, automatic defaults, overflow recovery, or universal support by
compatible endpoints. Earlier Opus 5 fixture attempts returned refusals; the
successful Opus verification used 5.5 as requested. No live tests were rerun to
write this document.

## Policy and operation are separate

Policy decides when compaction may happen automatically. An operation decides
how a particular compaction is executed. Manual requests and overflow recovery
are standalone operations even when the normal policy is ProviderTriggered.

| Requested setting | OpenAI Responses | Anthropic Messages | Chat Completions |
| --- | --- | --- | --- |
| Engine default | Engine-managed native standalone when supported | Engine-managed native on-demand when supported | Engine-managed Lightspeed standalone summarizer |
| ProviderTriggered | Native generation compaction, optional threshold override | Native threshold compaction, optional threshold override | Reject as unsupported |
| ProviderStandalone | Native compact operation | Native on-demand operation when supported | Lightspeed standalone summarizer |
| Disabled | No proactive compaction or automatic recovery | Same | Same |
| Explicit API compact, in any mode | Native standalone when supported | Native on-demand when supported | Lightspeed standalone summarizer |

Resolve Engine default conservatively from actual capabilities. A compatible
API shape or a recent model name does not establish native compaction support.
If native standalone compaction is unavailable, Engine default uses the
Lightspeed standalone path. An explicitly requested unsupported
ProviderTriggered mode fails validation instead of silently becoming another mode.

ProviderTriggered and engine-managed standalone are separate automatic
strategies. Do not proactively schedule standalone work over a healthy native
triggered generation. Its standalone path is used for explicit API compaction
and context-length recovery, with provider-specific strategy transitions only
where required. Full native triggered support is part of the first version.

Keep provider identity and API kind pinned. Fallback does not select another
provider, endpoint, credential, or model. Use the effective generation model
for the operation and validate model overrides against retained native state.
Switching models within a pinned route must not silently drop opaque state or
invalidate signed thinking.

The current ProviderStandalone name already covers summary generation for
Chat Completions. Keep compatibility with existing clients; exact DTO naming
and any added effective-policy projection are implementation decisions.

## Defaults and thresholds

### Provider-triggered

No user threshold means use the supported provider default, while still sending
the configuration that enables compaction. Compaction is not enabled merely by
omitting its settings from an ordinary request.

- OpenAI Responses uses `context_management` with a compaction edit. Retain the
  existing optional-threshold lowering and verify absent-threshold behavior
  against supported models as part of full mode support. Do not infer a numeric
  OpenAI default from Anthropic's value. If a route requires a threshold, the
  adapter supplies a verified model-aware default without requiring user input.
- Anthropic Messages uses `compact_20260112` in `context_management.edits` and
  the `compact-2026-01-12` beta. Its documented default is 150,000 input tokens;
  explicit triggers must be at least 50,000 tokens.
- Explicit thresholds must satisfy both provider validation and the selected
  model's usable input budget. Reject an unusable override instead of enabling
  a trigger that cannot run before the request exceeds the window.

Provider-native request fields and beta headers belong to adapters. Reject
conflicting raw provider parameters rather than allowing them to bypass the
configured policy, especially Disabled.

### Standalone

With known limits, no user threshold means an engine-managed proactive
threshold, not manual-only behavior. A supplied threshold overrides that
proactive threshold; overflow recovery remains enabled in either case.

Compute the usable input budget from the model's context window, output and
reasoning reservation, request overhead, and safety margin. Count current
instructions, tools, media, and retained native state as well as conversation
text. Avoid using cumulative billed usage as the current window size; cache
usage and compaction iterations need provider-specific interpretation.

Evaluate pressure before generation at safe turn boundaries, including between
tool rounds within a long run. Idle maintenance remains useful but cannot be
the only trigger. Large new input can require compaction before its first
generation. Protect that input rather than immediately summarizing it away.

Resolve numeric capacity outside the engine for the effective model and route:
explicit model/endpoint limit override, provider-reported limits, then verified
limits for supported model versions. Preserve the distinction between a
separate input limit and a total window shared with output; do not subtract
output twice. Record the resolved limits and their source as deterministic
facts. Resolve a model override independently. Do not make replay read mutable
discovery metadata or introduce an extensive model-catalog subsystem.

Occupancy describes the next rendered request. Use previous provider usage as
a baseline, account for newly added content and changed instructions/tools,
and obtain more accurate adapter counts where available and necessary. The
engine compares recorded capacity and occupancy; it performs no tokenizer or
provider I/O. Missing per-entry estimates must not disable recovery.

For unknown limits without an explicit threshold, let the session run until a
typed context-length failure and recover through standalone compaction. Do not
require a guessed limit to admit the provider. After successful recovery,
observed request sizes may support a conservative session-specific proactive
trigger, clearly marked as inferred rather than an exact discovered limit.
That inference is optional tuning; error-driven recovery is required in v1.
The initial threshold fraction, safety margin, summary budget, and recent-tail
budget need explicit implementation defaults and tests; this proposal does not
claim measured optimal values.

Treat `target_tokens` as a supported hint or summary budget, not a guaranteed
native output size. Check whether the resulting next request actually fits.

## Native standalone operations

### OpenAI Responses

Use `POST /responses/compact`. The request must fit the selected model's context
window, so an already oversized history requires bounded input preparation.

Preserve the complete returned compacted window, including retained messages
and other items alongside the encrypted compaction item. Do not filter it down
to only compaction items or apply the ordinary provider-triggered prune rule
to items the endpoint deliberately retained. Record which submitted prefix
the result replaces; a tail omitted from that request follows its result.
Continue to supply current Lightspeed-owned instructions and catalogs according
to native request semantics.

The encrypted item is provider state, not text to feed to the Lightspeed
summarizer. Prefer native re-compaction for windows carrying it. A lower-fidelity
fallback can use retained source history where available, but must report
failure if it cannot preserve the required state within its budget.

### Anthropic Messages

Use the ordinary Messages endpoint with top-level
`compaction: {"type": "summarize"}` and the `compact-2026-09-04` beta. This
is a native on-demand operation, not a separate compaction URL or an appended
user summarization instruction.

Send the matching system prompt and tools. Strip generation-only parameters
that the operation rejects. Validate the stop reason and a usable returned
block; a successful HTTP response alone does not establish successful
compaction. Preserve the block's content, signature, and native fields exactly.
Send the beta on subsequent requests carrying the block. Replace the submitted
messages and place the signed block first, followed by the untouched recent
tail. Respect the provider's conditions for retaining thinking.

**Threshold compaction cannot run on a request carrying an on-demand signed
block.** Manual native compaction or native overflow recovery in a
ProviderTriggered session therefore changes its effective execution strategy:
while that block is active, use engine-triggered native standalone compaction
and omit the threshold edit from subsequent generations. Keep the requested
configuration unchanged and record the transition as a provider-neutral fact.
Expose the effective strategy and reason to clients. The same restriction
applies when a user enables ProviderTriggered after an earlier manual compact.

After a manual compact in Disabled, replay the block correctly but perform no
later automatic compaction. The mode transition must never override Disabled.

Validate interoperability with earlier threshold blocks and model overrides
through adapter and live tests. Do not treat the old and new blocks as
interchangeable simply because both use native type `compaction`.

### Chat Completions and native-operation fallback

Use a dedicated summary call on the selected route, without tools or task
execution. Preserve goals, constraints, decisions, identifiers, unique findings,
unfinished work, and references needed to retrieve details. Record the result
as Lightspeed semantic context, distinct from native opaque state.

Lower roles, token budgets, and reasoning parameters according to the provider.
DeepSeek's system role and token fields must not inherit OpenAI-specific
assumptions merely because both use Chat Completions.

Use this operation when a native standalone capability is unavailable or a
bounded native attempt cannot fit. Authentication errors, rate limits, generic
invalid requests, and refusals must not be reclassified as context pressure.
Do not retry refusals through alternative prompts as overflow recovery.

## Selecting and reducing context

The first version uses protected-tail, chunked compaction for proactive,
manual, and error-driven standalone operations. Automatic tool-result clearing
is deferred: it introduces another information-loss policy and is not needed
to establish the shared lifecycle.

Runs are not retention units or compaction boundaries. A run may last hours,
contain many model/tool exchanges, and compact repeatedly before completing.
Every compaction operates on the current context revision at a safe generation
boundary without ending or restarting the active run.

All standalone operations share a deterministic selection plan:

1. Preserve current canonical instructions and catalogs independently of the
   summary. Protect unconsumed run input, steering, pending tool interactions,
   and the current task's recent complete exchanges.
2. Select a recent tail using complete model/tool exchanges and a token budget,
   never a count of runs. A user turn can include many exchanges; retaining
   the entire current run or user turn would leave hours of work uncompactable.
   Keep a tool call and all its answers on the same side of a cut, with stricter
   recorded request boundaries where provider thinking bindings require them.
   Never silently truncate protected new input to make room. Preserve this
   tail from the first attempt, not only after another strategy fails.
3. Divide the older prefix into bounded, coherent chunks at provider-valid
   generation/tool boundaries. A prefix that fits may be one chunk. For unknown
   capacity, reduce a rejected chunk at those boundaries and retry within the
   operation budget. The original generation overflowing and a compaction
   request overflowing are distinct observations with different next steps.
4. Build a rolling compacted prefix: compact the first chunk, then compact its
   result with the next chunk, until the selected prefix is covered. Include
   any previous session compaction state in that prefix. Each adapter must
   support and test this accumulation using its native continuation rules.
   Do not concatenate independent signed/encrypted artifacts or feed opaque
   native state to a text summarizer. Stage results without pruning source
   entries; finish with one valid compacted prefix or native compacted window.
5. Assemble canonical context, compacted prefix, recent tail, and current input
   using each adapter's native ordering rules. Validate the resulting request
   with sufficient headroom before committing and resuming generation.

Native summaries must preserve provider continuation rules. Client-side
history changes can invalidate Anthropic thinking bound to earlier content;
prefer native on-demand summaries where they preserve the kept thinking. When
a supported repair requires discarding incompatible thinking, record that
loss and apply the established continuation policy in the adapter. Never
rewrite signatures, opaque reasoning, or assistant tool-call records in place.

If the smallest provider-valid chunk cannot fit, or a rolling native artifact
cannot be combined with the next chunk within the budget, return a clear
failure. Do not silently remove tool evidence to force success. Supporting
oversized individual entries through further content splitting is separate
adapter work, subject to the same information-preservation and budget rules.

## Context-length recovery lifecycle

Recovery starts only from a normalized context-length rejection or equivalent
provider context-limit finish. Preserve that typed fact across the client,
runtime, worker, turn, and run boundaries. Ordinary request rejections retain
their existing visible failure behavior.

```text
generation reaches a context limit
  -> record the normalized failure and pause generation for the same run
  -> check effective policy; Disabled ends with the context-limit failure
  -> freeze the context revision and protected entries
  -> select an older prefix and protect recent complete model/tool exchanges
  -> compact the prefix through bounded rolling native or Lightspeed requests
  -> validate and atomically commit the replacement and any strategy transition
  -> retry generation with the same run and already-recorded tool results
```

Use explicit per-recovery attempt, summarization-call, token/cost, and elapsed
budgets. Require measurable reduction in the next rendered request; reject
empty, truncated, invalid, or non-progressing results. Transport retries remain
separate from the engine's context-recovery budget. Budget exhaustion produces
a clear terminal failure instead of another identical generation loop.

If canonical context and protected input alone exceed the usable window,
automatic compaction cannot solve it. Report that condition without deleting
the protected content. Cancellation and run deadlines remain effective while
recovery is executing.

Only one context mutation may commit at a time. Guard results with the base
revision and recorded covered range; stale results must not erase new input or
steering. An activity retry or workflow replay must not apply a result twice.
Intermediate chunk summaries do not prune source entries. Commit a validated
replacement atomically after all required stages succeed; otherwise leave the
previous active context available for inspection and repair.

## Manual API, projections, and compatibility

`session/context/compact` means perform one standalone operation, independently
of automatic policy. Disabled therefore suppresses automatic compaction only.
It does not remove native artifacts already needed for continuation, and an
explicit compact does not enable future automatic compaction.

Admit manual requests at a safe boundary. If work is active, queue the operation
until the current generation and pending tool batch have settled; do not mutate
their frozen context. Preserve existing API admission/result conventions and
make duplicate pending requests explicit. Start from the context revision at
execution, or reject a caller-supplied stale expected revision. Define queued
manual ordering relative to later run requests so compaction cannot starve.

Expose requested mode, effective strategy, effective threshold and its source,
trigger (manual, proactive, or context-limit recovery), progress, and failure.
Show before/after estimates where available, attempt counts, chunk progress,
and any thinking loss. Clients need readable compaction markers,
not signed or encrypted payload dumps. Retain usage for standalone calls and
native compaction iterations without double-counting generation usage.

Changing omission from Disabled to Engine default is a behavior change for
existing sessions and profiles. Lightspeed is early with few users: prefer a
deliberate, focused configuration upgrade over extensive compatibility
machinery. Preserve historical replay semantics and existing session history;
changing the default does not authorize rewriting prior events. New
session/default resolution follows the established explicit configuration,
profile, and inheritance rules. Record effective capabilities and policy facts
at deterministic boundaries; replay must not consult today's model catalog.
Do not silently reinterpret old omitted settings as permission for new paid
summary calls. Explicit Disabled remains authoritative in every version.

## Implementation progress

The engine now resolves omitted policies to standalone on new session and
configuration admissions. Historical omitted policies keep their original
Disabled replay behavior; replacing configuration opts that session into the
new resolution. Explicit Disabled remains unchanged. No historical events are
rewritten.

The shared standalone lifecycle runs between generations, including multiple
operations within one run. It freezes the covered prefix and revision, replaces
that prefix atomically on success, and keeps the two newest settled generation
exchanges and their immediately preceding user input where older history exists.
With no older prefix, explicit compaction can cover the settled window. A second
consecutive context-length failure expands coverage to the whole settled window;
unconsumed current input and unanswered tool exchanges cannot be discarded.
Recovery allows two attempts per consecutive overflow sequence, resets after a
successful generation or a new run, and preserves the exact terminal provider
error when its budget is exhausted. Partial rejected generation output is not
committed or executed. Manual requests queue without moving an in-flight
request's revision; cancellation can abandon a pending compaction.

The runtime rolls bounded complete chunks into one replacement window. Defaults
are 32 calls, 2,000,000 cumulative input tokens, and a 600-second operation
deadline. Typed context-length rejection shrinks a chunk at a safe boundary;
ordinary invalid requests, authentication failures, and refusals abort. A
smallest atomic exchange that cannot fit fails clearly and leaves source context
intact. Previous native compacted windows remain indivisible. Tool-result
clearing, chunk telemetry events, and learned thresholds are deferred.

Capacity is resolved outside the reducer and recorded separately from the
user's optional `context.inputLimitTokens` override. Anthropic model discovery
supplies reported input capacity. OpenAI and custom routes whose discovery does
not supply it remain unknown; they use error-driven recovery unless an override
is supplied. The default proactive threshold is 80% of known usable input
capacity. Run overrides use their own resolved capacity, and never inherit a
limit from a different model. Retained signed, encrypted, or reasoning state
prevents incompatible model changes.

OpenAI standalone retains every item in the returned window, in order. Supported
Anthropic models use native on-demand compaction, retain its exact signed block,
and select the on-demand beta on replay. A retained signed standalone artifact
suppresses Anthropic threshold compaction while preserving the requested policy.
Older Messages models and all Chat Completions routes use ordinary summary
generations. Explicitly unavailable native endpoints (unsupported operation or
HTTP 404/405/501) fall back to a summary call on the same route. Generic HTTP 400
errors do not authorize a fallback. Native requests retain current tool/catalog
configuration, and Anthropic provider-hosted MCP auth is resolved at send time.

API projections expose requested/effective mode, strategy, threshold/source,
capacity, observed token usage when current, pending/queued state, recovery
attempts, and finished-call usage/counts. Session settings show effective mode,
threshold source, pending/queued status, attempts, and the Anthropic transition.
Public contracts and TypeScript consumers have been regenerated. Fine-grained
per-chunk progress and provider-reported thinking-loss telemetry remain follow-up
observability work; the engine never rewrites the preserved tail's raw entries.

## Architecture and implementation sequence

The deterministic engine owns policy facts, protected ranges, safe boundaries,
recovery budgets, revision guards, and context events. It emits intents for
counting and compaction where effectful work is required. Capability discovery,
tokenizers, provider calls, native JSON, beta headers, CAS reads/writes, and
transport configuration stay in effectful adapters and activities. Record only
provider-neutral observations needed to replay decisions.

Implement through existing runtime, in-process runner, and hosted worker
boundaries rather than adding a separate orchestration framework.

- [x] Review existing implementation and provider contracts.
- [x] Live-verify existing triggered and standalone paths as recorded above.
- [x] Define capability resolution, Engine default upgrade semantics, and
  requested/effective policy facts; implement validation and projections.
- [x] Preserve typed context-length failures and add a replayable recovery
  lifecycle without terminalizing the run before recovery is considered.
- [x] Preserve the full OpenAI standalone output; implement native Anthropic
  on-demand requests, signed-block replay, and effective strategy transitions.
- [x] Add protected-tail selection within active runs, bounded rolling chunks,
  repeated compaction, and summary validation. Defer tool-result clearing.
- [x] Retain full OpenAI/Anthropic provider-triggered lowering, capture, pruning,
  continuation, and usage support alongside standalone defaults and recovery.
- [x] Add model-aware standalone thresholds and safe pre-generation triggers,
  including active runs and model overrides.
- [x] Allow manual compaction in all modes with safe scheduling and revision
  guards; expose policy, recovery, and usage in API/UI/CLI projections.
- [x] Regenerate public contracts and workflow consumers; record implementation
  and validation here. Broader user-guide changes remain subject to user review.
- [x] Run replay, integration, and authorized live validation for the new paths.
- [x] Group context bookkeeping by ownership: compaction phase and completion
  markers, retained generation metadata, revision-bound usage observations, and
  run-owned overflow recovery.

The state refactor on 2026-10-02 retains the Requested, Queued, and Finished
events. `ContextState` now has five fields: revision, entries, compaction,
last generation metadata, and an optional usage observation. Compaction uses
Idle, QueuedManual, or Pending with a required frozen plan; requests also require
that plan, without an older planless-request compatibility path. Model and input
capacity remain available after a run ends, while token observations are usable
only at their recorded context revision. Recovery attempts and the recovered
turn belong to the active run, so a subsequent run starts with a fresh budget.
Queued and pending snapshot restoration, observation replay and invalidation,
and a new run after exhausted recovery are covered by deterministic checks.

The frontend transcript retains original conversation history while suppressing
standalone replacement entries, including native messages, tool copies, and
text summaries. A single operation marker progresses through queued, compacting,
and succeeded or failed states; provider-triggered compaction retains its native
marker. Run statistics include reported standalone usage and call counts while
keeping the last generation's context measurement separate. Older-page loading
preserves a live marker's identity and recovers its run attribution without
replaying old lifecycle events into live controls. Session closure interrupts
unfinished progress. Validation on 2026-10-02 passed all 688 frontend tests,
TypeScript checking, and the production frontend build.

Two seeded demo conversations now exercise the transcript and settings on
2026-10-02. Software Factory's LIN-1421 implementation thread uses Engine
default standalone with a 10,000-token input override and the derived 8,000-token
threshold. It compacts inside one run before opening the PR, keeps the last two
completed tool exchanges unchanged, and includes the compaction call in run
usage. Personal Assistant's Ada Telegram thread uses provider-triggered
compaction with an explicit 50,000-token threshold; its native artifact appears
inside an ordinary generation and later work retains the promised cohort cut,
references, and send-approval requirement. Both examples use Opus 5.5 and keep
the original transcript history. Demo policy edits refresh the settings
projection. These are simulated provider artifacts, not additional live-provider
evidence. All 691 frontend tests, TypeScript checking, and the demo build passed.

## Implementation validation

State-refactor checks on 2026-10-02 passed: 788 scoped library tests across the
engine, API projection, workflow, hosted server, and in-process runner; the exact
workspace/all-targets Clippy gate with warnings denied; and all 14 rerun live
cases. The live checks comprise the 12 dedicated OpenAI Responses, Anthropic
Opus 5.5, and OpenAI/DeepSeek Chat Completions compaction tests plus the two
serialized hosted standalone-compaction and continuation tests.

Completed checks on 2026-10-01:

- Cross-crate harness, runtime, provider client, API projection, workflow, and
  in-process runner tests, including replay, protected tool exchanges, unknown
  capacity recovery, partial-output discard, cancellation, manual queueing,
  exact full native windows, same-route fallback, and capability selection.
- Hosted runtime library tests: 359 passed, one unrelated credentialed test
  ignored. Additional projection and runtime budget tests passed.
- Exact workspace gate: `cargo clippy --workspace --all-targets --locked -- -D warnings`.
- TypeScript typecheck, consumer suites, all 672 web tests, and production/demo
  builds. Regenerated artifacts were checked for repeatable output.
- OpenAI Responses live suite: four passed, covering triggered capture/pruning
  and continuation, omitted threshold, manual native standalone, and proactive
  standalone.
- Anthropic Messages live suite using `claude-opus-5-5`: four passed, covering
  native on-demand manual/proactive compaction, rolling chunks with unchanged
  native recent turns and a triggered-to-standalone transition, and native
  triggered capture/pruning/continuation.
- OpenAI GPT-5.5 and DeepSeek V4 Pro Chat Completions live compaction: four passed
  across summary fact retention and continuation.

A subsequent authorized rerun passed all 18 selected live tests: the 12 dedicated
provider compaction cases, four direct adapter/compatibility checks, and two new
provider-backed hosted tests through local Temporal, PostgreSQL, and the object
store. The hosted cases cover manual compaction in Disabled, proactive
standalone, retained native artifacts, continuation with exact fact recall, and
finished-call usage. They wait for compaction's own completion independently of
run completion. Opus uses 5.5, including the shared live fixture default.

The rerun corrected older Anthropic fixtures that expected a plain-text summary
and a binding rejection during native whole-window compaction. The updated
checks verify exact signed output and successful strict continuation from the
replacement; strict rejection of edited preserved thinking during ordinary
generation remains covered. Native capability selection also includes the
currently documented Sonnet 5.5, Fable 5.1, and Mythos 5.1 model identifiers.

Rare overflow and refusal outcomes use deterministic/synthetic fixtures. These
passes do not claim live verification of every compatible custom endpoint or
the complete credentialed Temporal test matrix. Unknown capacity remains deliberately
reactive, and an oversized indivisible exchange fails without silently dropping
history.

## Validation and acceptance

Unit and replay tests must cover the full mode/API-kind matrix, explicit
Disabled, default inheritance and legacy configurations, capability changes,
threshold omission/override, model overrides, and raw-parameter conflicts.

Engine vectors must cover recent-tail and tool-pair invariants; new input larger
than the budget; large single-turn tool runs with multiple compactions before
run completion; rolling-prefix coverage; stale revisions and steering;
successful recovery and bounded failure; cancellation; duplicate results; and
reconstruction of effective strategy transitions from events alone. Completed
tool executions must not be repeated by recovery.

Adapter tests must cover native request schemas, absent thresholds, exact
signed/opaque replay and repeated native accumulation, full OpenAI compact
output, Anthropic header selection and invalid stop reasons, thinking
continuation, per-iteration usage, and
provider-specific Chat Completions lowering. Native and Lightspeed fallback
paths must both fail clearly when they cannot preserve usable context.

Integration tests must verify manual compaction in Disabled and
ProviderTriggered, safe active-run scheduling, proactive standalone without a
user threshold when limits are known, error-driven recovery with unknown
limits, HTTP and in-band context-limit recovery, repeated compaction during
one run, and visible final failures. Verify normal provider-triggered operation
does not schedule competing proactive standalone work. Test both in-process
and hosted execution boundaries. Use synthetic
provider fixtures for rare/error outcomes rather than relying on live refusal
or rate-limit behavior.

Add ignored live tests for OpenAI Responses native trigger/default and
standalone continuation, Anthropic Opus 5.5 native on-demand and its transition
from threshold mode, and OpenAI/DeepSeek Chat Completions bounded summarization
and continuation. Include tool-pair and kept-thinking cases where supported.
Run live tests only when authorized; serialize Temporal suites. Earlier live
passes do not satisfy these new recovery acceptance tests.

Completion requires scoped Rust and web checks, replay coverage, regenerated
contracts where necessary, and the exact workspace Clippy gate before a PR.
No production rollout should depend on an unverified native compaction default.

## Evidence and remaining tuning

Provider documentation reviewed during the design discussion on 2026-10-01:

- [OpenAI compaction](https://developers.openai.com/api/docs/guides/compaction):
  generation-time compaction, standalone input limits, and preserving the full
  returned compacted window.
- [Anthropic threshold compaction](https://platform.claude.com/docs/en/build-with-claude/compaction-threshold):
  explicit enablement, the 150,000-token default, and the 50,000-token floor.
- [Anthropic on-demand compaction](https://platform.claude.com/docs/en/build-with-claude/compaction-on-demand):
  native request, signed-block replay, error handling, supported models, and
  incompatibility with threshold compaction.
- [Anthropic keeping recent turns](https://platform.claude.com/docs/en/build-with-claude/compaction-keep-recent-turns)
  and [preserved thinking](https://platform.claude.com/docs/en/build-with-claude/compaction-thinking-blocks):
  safe cut points, unchanged tails, and thinking-binding conditions.
- [Anthropic context editing](https://platform.claude.com/docs/en/build-with-claude/context-editing):
  oldest tool-result clearing with keep/exclude controls and cache tradeoffs.
- [OpenAI session memory examples](https://developers.openai.com/cookbook/examples/agents_sdk/session_memory):
  complete-turn trimming and older-history summarization with a recent tail.

Initial numeric budgets, capacity discovery, native capability selection, and
public projections are recorded in Implementation progress. Measure compaction quality,
fact retention, latency, cost, and cache effects before tuning those defaults.
The policy matrix, engine-managed standalone defaults, full provider-triggered
support, Disabled semantics, manual override, native operation preference,
protected-tail chunking within active runs, and bounded context-length recovery
are the agreed behavior. Automatic tool-result clearing and learned thresholds
remain outside the first version's required scope.
