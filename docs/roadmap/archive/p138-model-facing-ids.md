# P138 — Model-Facing Ids: Counters and Names, Never Hashes

**Status**

- Proposed 2026-08-26, from the question "what kinds of ids does the model
  have to pass around?" Weaker models drop and transpose characters in long
  ids; a mistyped promise id costs a turn. The facts below were verified in
  code the same day.
- Core tier: `harness` (`PromiseId`, `IdCursors`, the tool batch request,
  promise effects), `tools` (concurrency, workflow-tool adapter, job
  arguments), `temporal-runtime` (session tool runtime, job and sub-agent
  activities), `temporal-workflow` (reply tokens, the source-resolution
  emission id), `api` (promise views, key-source input), and both contract
  exports. Core only: the bot-side ids (`bot_emit`'s return, the digest
  ids shown by `bot_event_list` / `bot_status`) are
  [P135 §8](p135-bot-federation.md), its first slice.
- Builds on P100b (keyed promise sets), P134 (`agent_spawn`), P113 (job
  tools). Orthogonal to [P136](p136-context-catalogs.md) and
  [P137](p137-prompt-caching.md): ids live in tool results at the tail of
  the context, so nothing here touches the cached prefix.
- Greenfield: promise ids change shape without back-compat; dev databases
  reset; the workflow contract export regenerates.
- **Implemented 2026-08-26** (slices 1–3, one pass):
  - Engine: `PromiseId` is a validated `promise_<n>` string with a numeric
    accessor and numeric ordering (one string in the log, emissions, API,
    and tool arguments — no boundary conversions); `IdCursors.last_promise_id`;
    `ActiveToolBatch::promise_id_base` recorded at batch creation;
    `promise_id_base` on batch and per-call requests (a per-call dispatch
    owns slot `base + index`); the drive rejects a minted id below the
    batch base or already held; `workflow_tool_promise_id` and the
    canonical-derivation check are gone (keyed maps must be distinct);
    `WorkflowToolCompletionKeySource::ArrayItemField`; `PromiseIdAllocator`.
  - Executors: one allocator per batch dispatch in the session tool
    runtime, threaded into `invoke_workflow_tool` and `sleep`;
    model-visible acknowledgements are `{"accepted":true,"promise":…}` /
    `{"accepted":true,"promises":{…}}` with invocation/execution ids kept
    in `output_json`; `await`/`cancel`/`detach` parse `promise_<n>` and
    turn a malformed id into a tool error.
  - Jobs: `job_submit` bound with `ArrayItemField { "/jobs", "job_id" }`,
    subscriptions keyed by the model's `job_id`, `job_id` validated to the
    completion-key shape (schema pattern + argument check); job handles
    default to the active environment; `JobSubmitted.handle` populated.
  - Contract: `sourceResolution` emission ids include the holder workflow
    id (`WORKFLOW_CONTRACT_VERSION` 2); Rust and TS derivations, the
    export, the TS client, and the bot/channel producers updated.

## Goal

Every id the model must read back to Lightspeed is short, mostly
alphanumeric, and — where the model named the thing — the model's own name.
Digests and UUIDs stay where they belong: durable identity, idempotency
keys, producer correlation. The rule: **the model copies counters and
names, never hashes.**

## Facts today (verified 2026-08-26)

**Engine ids.** Every engine-native identifier is a session-scoped counter
allocated from `IdCursors`: `RunId`, `TurnId`, `ToolBatchId`,
`ContextItemId`, `SteeringId`, `EventSeq` (`numeric_id!` in
`engine::core::components::ids`), rendered by `api-projection` as `run_7`,
`turn_3`, `item_12`. **`PromiseId` is the one exception**: a free-form
string "minted by the tool executor that creates the promise (a
deterministic digest of the creating call context)". The executor picks
it, `promise_from_create_effect` records whatever the create effect
carries, and the reducer enforces only uniqueness.

**What the model sees and must echo.**

| Surface | Shape | Chars | Model copies it back? |
|---|---|---|---|
| `agent_spawn`, `job_submit`, plugin workflow tools → promise | `wtp:sha256:<64 hex>` (`workflow_tool_promise_id`) | 75 | **Yes**, into `await` / `cancel` / `detach`, up to 32 at a time |
| the same acknowledgement, `invocationId` | `wti:sha256:<64 hex>` | 75 | No — nothing accepts it |
| the same acknowledgement, `executionId` | `wtx:sha256:<64 hex>` | 75 | No — nothing accepts it |
| `sleep` → promise | `promise_timer_<32 hex>` (`timer_promise_id`) | 46 | **Yes** |
| `job_submit` promise map keys | `job-0`, `job-1`, … by array index (`ArrayIndices { prefix: "job-" }`) | — | Model maps index → its own `job_id` → promise |
| `job_read` / `job_cancel` handle | `{ environment_id: "environment_<32 hex>", job_id }` | 44 + | **Yes**, although `job_submit` rejects an environment override |
| `job_run` job id | `job-<24 hex>` (`derived_job_run_id`) | 28 | Yes, for `job_read` |
| `environment_list` / `environment_select` / `environment_read` | `environment_<32 hex>` (uuid v4, simple) | 44 | **Yes**, to select |
| `run_process` → handle for `write_process_stdin` | `proc-<pid>-<19-digit nanos>-<n>-<n>` | ~35 | Yes |
| sub-agent result envelope `session_id` | `agent_<32 hex>` | 38 | No |
| VFS catalog routes | `snapshot_ref: sha256:<64 hex>`, `workspace_id: workspace_<32 hex>` | 71 / 42 | No — addressed by `path` |
| skill catalog | `skill:<16 hex>:<16 hex>` | 39 | No — read by path |
| bot events | `#<seq>` small integers (`bot_event_read #12`) | 1–3 | Yes — **the good exemplar** |
| `bot_emit` return | `eventId: self-<uuid>` | 41 | No — P135 §8 returns `seq` |
| tool call ids | provider-native `toolu_01…` / `call_…` | ~28 | No — the provider round-trips them |

One `agent_spawn` acknowledgement is therefore three 64-hex digests, of
which the model needs exactly one:

```json
{"accepted":true,
 "invocationId":"wti:sha256:6b0c…(64)","executionId":"wtx:sha256:9e21…(64)",
 "promises":{"reply":"wtp:sha256:1f4a…(64)"}}
```

**Where the digest matters.** Three consumers depend on the promise id:

1. the engine's `PromiseComponentState.promises: BTreeMap<PromiseId, _>`
   (needs uniqueness within the session);
2. the producer's reply token — `RunTerminalNotifyIntent.token` for
   sub-agents, `EnvironmentJobSubscription.promise_id` for jobs,
   `completion_promises` in the emission for plugin receivers; the holder
   maps the token back with `PromiseId::new(token)` (needs uniqueness
   within the holder workflow, which is the addressee);
3. the `sourceResolution` emission id, derived from
   `(universeId, producerWorkflowId, promiseId)` in the exported workflow
   contract (needs uniqueness across everything one producer resolves).

The stated rationale — "so replay re-derives identical ids" — does not
hold: the executor is a Temporal activity whose result is recorded in the
session log; replay reads the log and never re-runs it. What the digest
really buys is (2) and (3), and `PromiseSource::Workflow` already carries
`invocation_id` + `completion_key` separately, which is the durable
correlation a producer needs.

**Cost.** Random hex tokenizes at roughly two characters per token: ~100
tokens of hex per spawn, ~1 000 tokens of ids in a 32-promise `await`,
repeated in the result. The larger cost is copy fidelity. The failure is
at least soft — `await` returns `invalid_await_tool_result` for an unknown
id and `cancel` / `detach` see `PromiseControlStateRuntime::Unknown` and
fail the call — but it burns a turn, and weak models loop on it.

## Proposed

### 1. `PromiseId` becomes a session counter

A session counter allocated from a new `IdCursors.last_promise_id`,
rendered as `promise_7` — the same convention as `run_7`, one string at
the tool surface, in await results, in `PromiseCreated` notifications and
`PromiseView`. (`p7` is shorter; a second convention is not worth it.) As
built, `PromiseId` stays a string newtype that validates the
`promise_<n>` shape and exposes the number, rather than a `numeric_id!`:
the same value then travels through the log, emissions, the API, and tool
arguments without conversions at every boundary.

The acknowledgement text is written by the executor before the engine
applies the event, so the executor must know the number:

- **Engine.** The batch records `promise_id_base = last_promise_id + 1`
  when it is created; the drive copies it onto
  `ToolInvocationBatchRequest`, and a per-call dispatch (P114) gets slot
  `base + call index`, since a per-call tool creates at most one promise.
  Turning a result's effects into events checks `id >= base` and not
  already present; `Promise(Created)` apply bumps the cursor to
  `max(last, id)`. Deterministic, because the batch result is a recorded
  event, and order-independent, because the base — not the live cursor —
  is the floor.
- **Executors.** A per-batch allocator seeded from the base hands out
  `base + k`; `invoke_workflow_tool` and `invoke_sleep_call` take it. Parallel
  calls receive numbers in nondeterministic order — harmless, since only
  the recorded attempt's result is ever applied; an activity retry gets the
  same base and a fresh counter.
- **Producer side.** `PromiseSource::Workflow` keeps `invocation_id` +
  `completion_key`. Reply tokens and `completion_promises` carry
  `promise_7`. Receivers already treat the value as opaque (bots:
  `replyPromiseId(invocation)`; channels: `envelope.body.promise_id`).
- **Contract.** A session-scoped id makes two holders' `promise_7` collide
  under one shared producer (a bot controller, a channel receiver) in the
  `sourceResolution` emission id. Add `holderWorkflowId:utf8` to its parts
  — the envelope already addresses the holder. Regenerate
  `crates/temporal-workflow/contract/`; TS fixtures that spell
  `wtp:sha256:` update.
- **Unchanged.** `wti:` / `wtx:` / `wtb:` stay digests: they are Temporal
  idempotency keys and binding identity, never model input.

### 2. Acknowledgements show only what the model needs

`model_visible_text` of a workflow-tool acknowledgement becomes
`{"accepted":true,"promise":"promise_7"}` for a single key and
`{"accepted":true,"promises":{"build":"promise_9","test":"promise_10"}}`
for a keyed set; `output_json` keeps `invocationId` / `executionId` for
clients. `sleep` says "Timer scheduled for 5000 ms (promise promise_8)".

### 3. `job_submit` promises keyed by the model's `job_id`

`job_id` is already a required, validated field of every submitted job;
the `job-<index>` key is a gratuitous indirection. Add
`WorkflowToolCompletionKeySource::ArrayItemField { pointer, field }` —
`StringArray` only handles arrays of strings — and bind `job_submit` with
`{ pointer: "/jobs", field: "job_id" }`; the environment-job activity keys
its subscriptions by `job.job_id`. `WorkflowToolCompletionKeySourceInput`
and the API contract gain the variant.

Completion keys allow `[A-Za-z0-9_.-]` up to 64 bytes; `JobId` allows
`:` and 128. Constrain `job_submit`'s `job_id` to the completion-key shape
at argument validation so a key derivation can never fail on an accepted
job.

### 4. Job handles default to the active environment

`JobHandleArg.environment_id` becomes optional; the `job_read` /
`job_cancel` schemas mark it so; the executor fills it from
`active_environment_id`. Explicit ids stay for cross-environment reads.
`JobSubmitted.handle` is populated on the inline path (today `None`).

### 5. Environment names — a separate decision

Bots and profiles select by `^[a-z0-9][a-z0-9-]*$` names; environments
have only a non-unique `display_name` beside `environment_<32 hex>`. A
universe-unique `name`, accepted by `environment_select` and shown by
`environment_list`, would remove the last 44-character id the model
copies. Registry and API change, not core; not sliced here.

### 6. Small

- Sub-agent `session_id: agent_<32 hex>` in the result envelope stays: the
  model never copies it and the sessions tree reads lineage from the API.
- Process handles (`proc-<pid>-<nanos>-<n>-<n>`) are unique within a
  shared daemon namespace; a session-scoped alias needs a table the engine
  doesn't keep. Leave until it bites.

### Alternatives considered

- **Alias layer** (keep the digest as `PromiseId`, add an ordinal alias):
  the executor still has to know the number for the acknowledgement, two
  ids stay in sync forever, and the API shows both. Rejected.
- **Engine assigns after the fact, executors omit ids:** the acknowledgement
  is written before the engine applies, so the number cannot appear in
  it. Rejected.
- **Compound deterministic keys** (`p<run>.<batch>.<call>.<key>`): no base
  needed, but a four-part id is what this doc removes. Rejected.
- **Prompt guidance** ("copy ids exactly"): does not help the models this
  is for. Not a substitute.
- **Model-named promises** (`agent_spawn { handle: "review" }`): the
  workflow-tool contract deliberately keeps the model from naming promises;
  keys come from validated arguments. §3 gives this for jobs, where a name
  is already required. Out of scope for spawn.

## Slices

1. **Counter ids.** `IdCursors.last_promise_id`, the `promise_<n>` `PromiseId`,
   `promise_id_base` on the batch request and batch state, apply check,
   executor allocator, `sleep`, reply-token paths, `sourceResolution`
   derivation with holder, contract export, TS fixtures. The largest slice.
2. **Acknowledgements and job keys.** Slim `model_visible_text`,
   `ArrayItemField`, `job_id` shape aligned with completion keys, API
   input variant, contract export.
3. **Job handles.** Optional `environment_id`, populated `handle`.

## Tests

- `harness`: `Promise(Created)` below the base or duplicate is an invariant
  violation; the cursor bumps to the max; a recorded batch with
  out-of-order numbers reduces identically on replay; continue-as-new keeps
  the cursor.
- `tools`: acknowledgements contain only `promise` / `promises`; keyed maps
  use the `job_id`s; `ArrayItemField` rejects missing, non-string,
  duplicate, and invalid field values; `await` / `cancel` / `detach` accept
  `promise_<n>` and turn a malformed id into a tool error, never an
  invariant violation.
- `temporal-runtime`: parallel calls in one batch get disjoint ids; the
  sub-agent reply token round-trips; job subscriptions are keyed by job id;
  `job_read` without `environment_id` reads the active environment.
- `temporal-workflow`: two holders resolving `promise_1` through one
  producer get distinct emission ids; `cargo test -p temporal-workflow`
  fails while the export is stale.
- Live: `workflow_tool_plugins_live` parses `promise_<n>`; the sub-agent
  (spawn → await) and environment-job (submit → await keyed by job id)
  suites stay green.

## Non-goals

- No change to `wti:` / `wtx:` / `wtb:` / `msc:` fingerprints,
  `session_<32 hex>`, `EmissionId`, universe ids, or Temporal workflow ids.
- No aliases for VFS `snapshot_ref` / `workspace_id` or skill ids: catalog
  entries are addressed by path, and rewriting them only churns the prefix.
- No model-named promise handles.
- Provider tool-call ids are the provider's.

## Doc drift

[P134 §1](p134-subagents.md) says `agent_spawn -> { promise, sessionId,
runId }`. The code returns the generic acknowledgement, and the child
session id is not known at acknowledgement time (the execution's prepare
activity creates it). The acknowledgement is now `{ accepted, promise }`
and the child ids arrive in the result envelope; P134 §1 was updated with
this implementation.
