# P159 — Built-in Tool Registry and Late Resolution

**Status**

- Implemented and verified offline and live 2026-09-05.
- Completions presentation follow-up 2026-09-19: reuse shell command execution,
  omit patch tools from the generic default, retain explicit presentation
  overrides, and preserve already-admitted calls under the original format.
  Canonical patch descriptions document the grammar and an executable example;
  Codex-like patch descriptions stay concise, naming the `apply_patch` syntax
  and its begin/end markers. Both retain filesystem-scope and domain guidance
  and share their schema and implementation.
  Offline validation: tools and LLM runtime unit suites plus captured provider
  request contracts. No live provider tests were run for this follow-up.
- Simplification is the goal: stable internal identity, one resolver, matching
  execution, and preserved provider transcript. No versioned tool registry.
- The original registry refactor preserved model-facing behavior. The later
  Completions default change is tracked explicitly in request fixtures and
  retains narrowly scoped execution support for already-admitted calls.
  It does not add aliases to the advertised tool catalog or a second registry.
- Builds on the provider presentations in
  [P151](p151-exec-leftover-processes.md), the filesystem boundaries in
  [P113](archive/p113-explicit-vfs-and-environment-tool-domains.md), and the
  execution invariants in [P157](p157-native-mcp-in-mixed-tool-batches.md) and
  [P158](p158-anthropic-hosted-web-tools.md).

## Goal

The engine records the capabilities admitted to a session under stable internal
identities such as `env.run_process` and `vfs.read_file`. The LLM activity resolves
those registrations into the names, schemas, and adapters appropriate for the
actual model of that turn. Returned calls map back to internal identities before
the engine schedules execution.

Built-in descriptions and schemas remain code-owned and are rendered directly
at that boundary. They no longer need to be written to CAS, admitted as schema
refs, and read back for every model request.

```text
Session feature grants and trusted tool declarations
  -> admitted registry: internal identities, definition sources, policies
  -> LLM activity: resolve presentations for the effective turn model
  -> provider-native request plus request-local call bindings
  -> provider response: preserve native transcript, resolve call identities
  -> engine: accept and schedule calls by admitted identity
  -> tool activity: execute using the selected binding and render its result
```

## Previous Approach

`crates/tools/src/builtin/mod.rs` already separates logical operations from
presentations. `BuiltinTool` captures domain, operation, surface, variant, and
one-shot behavior. However, `spec_bundle` generates descriptions and schemas as
`ToolDocument`s and wraps their hashes in an engine `FunctionToolSpec`.

`gateway/service/session_toolset.rs` composes that toolset, writes its documents
to CAS, and patches `ToolingState.tools`, keyed by the final `ToolName`. The
LLM adapters then read the same documents to build native provider requests.
The engine never inspects the built-in schema contents. It uses tool membership,
execution policy, parallelism, provider compatibility, and tool choice.

Execution has a separate reconstruction path:
`worker/session_tools.rs::runtime_catalog` builds default toolsets for all three
API kinds and merges their name-keyed bindings. It also generates documents that
this path does not need. This loses the relationship to the presentation that
produced the call. In particular, admission can render a one-shot process tool,
while execution reconstructs `EnvironmentToolsetConfig::basic()`, which enables
continuation. The process adapter uses that option for waiting and timeout
defaults, so preserving only the public name is insufficient.

The current design pins description/schema bytes through CAS, but does not pin
the matching executable adapter. This refactor makes that relationship explicit.

## Decision 1: Registry Identity Is Internal

Keep one admitted, revisioned registry. Key it by an internal tool identity,
with separate fields for internal identity and provider-visible name. A built-in
registration identifies an operation; it does not store `Bash`, `exec_command`,
or a provider-specific schema as its identity.

For built-ins, the stable identities should follow the existing logical ids.
VFS and environment operations remain distinct. Workflow registrations retain
their trusted workflow-tool identities; MCP registrations retain server/link
identity. Custom tools may retain an authored name as presentation data without
making that string the execution authority.

The durable registration contains only what admission, scheduling, and later
resolution need:

- Internal identity and definition source.
- Small trusted options that change the admitted contract, including process
  continuation policy and capability-specific restrictions.
- Execution binding or eligibility, parallelism, and the existing execution
  class/retry-safety facts.

The type names below are illustrative; the separation is the contract:

```rust
ToolRegistry = BTreeMap<ToolId, ToolRegistration>

ToolRegistration {
    definition,       // Builtin reference, external definition, or MCP source
    execution,        // Local, workflow-bound, hosted, or admitted MCP policy
    parallelism,
    execution_policy,
}
```

Do not add a second model-name registry to engine state. Provider names and
concrete surfaces are resolved outside the engine. An explicit presentation
override, where supported today, is a resolution input; the provider default is
chosen from the effective turn model, including a run model override.

The engine owns the small protocol types, or they live in a dependency-light
protocol module. The `tools` crate owns the catalog of implementations, schema
builders, and codecs. `engine` must not depend on `tools`, resolve a built-in
schema, or acquire one reducer branch per built-in implementation.

## Decision 2: Definition Source and Execution Are Separate

Being built-in describes where a tool definition comes from. It does not imply
that execution happens inline in a tool activity.

| Tool family | Definition source | Execution remains |
| --- | --- | --- |
| VFS and environment file/process tools | Built-in reference | Domain-specific runtime |
| Concurrency and environment control tools | Built-in reference | Existing control/batch semantics |
| Local web fetch and provider-hosted web tools | Built-in reference with trusted options | Selected local or hosted implementation |
| System subagent and environment-job tools | Built-in reference | Generic workflow-tool binding |
| Externally declared workflow/custom tools | Authored definition refs | Admitted execution binding |
| Remote MCP tools | Admitted server source and runtime discovery | Existing native/provider MCP policy |

Apply this source distinction to all built-in definition producers, including
code-owned workflow-tool definitions, rather than removing schema refs from
filesystem tools while leaving the same detour elsewhere. External workflow
declarations supply immutable schema refs. The public `WorkflowToolKindInput`
accepts only authored functions: system subagent and environment-job bindings
are constructed directly inside the runtime, so their built-in definitions do
not need a public workflow-input variant. Read-only tool inventory still exposes
the built-in definition kind.

A definition authored in this repository is not automatically a session-runtime
built-in. Bot, channel, and plugin declarations supplied through the generic
workflow protocol may remain external definitions. Do not introduce dependencies
from the engine or LLM resolver onto those application implementations.

Workflow argument/output validation resolves the same definition source as LLM
presentation. It must not require `ToolKind::Function` with an input-schema blob
when the definition is built-in. Completion promises, receiver authorization,
start recipes, binding fingerprints, and reply contracts retain their generic
workflow semantics. A built-in definition never authorizes a new endpoint.

Provider-dependent web resolution must preserve the current routes: hosted
search on OpenAI Responses and Anthropic Messages, hosted fetch on Anthropic,
and guarded local fetch on the other supported routes. Required provider-native
options and request additions are produced with the resolved tool. Hosted calls
remain provider-owned; only client-executable bindings produce schedulable tool
calls. Preserve the small execution facts the engine needs to enforce this
distinction without teaching it provider wire schemas.

## Decision 3: Resolve a Complete Binding Inside the LLM Activity

Resolution consumes the admitted registry snapshot, trusted options, the actual turn model. It produces native
provider tools and a request-local reverse lookup from exposed name to binding.
It is shared by hosted execution and `crates/eval`.

A resolved binding associates:

- The admitted internal registration identity.
- The exposed name, description, input schema, strictness, and provider options.
- The selected argument decoder, result renderer, and presentation variant.
- The admitted execution route and contract options.

The provider request types remain native to their adapters. A common binding
resolver is not a new lowest-common-denominator provider request format.
Built-in rendering returns values directly; it does not create temporary CAS
blobs just to reuse the old function-tool materializer.

### Presentations and expansion

OpenAI Responses uses the Codex-like surface and Anthropic Messages uses the
Claude-Code-like surface. OpenAI Completions reuses the Codex-like shell tools
and neutral filesystem schemas, but its provider-default resolution omits
`env.apply_patch` and `vfs.apply_patch`. Exact-text editing remains available.
Explicit Codex-like and Canonical overrides retain patch tools when admitted;
Canonical also retains `run_process(argv, ...)` and `continue_process`.
There are no model-name heuristics or new public configuration fields.

| Internal identity | Canonical | Codex-like | Claude-Code-like |
| --- | --- | --- | --- |
| `env.run_process` | `run_process` | `exec_command` | `Bash` |
| `env.continue_process` | `continue_process` | `write_stdin` | `BashOutput`, `KillShell` |
| `vfs.read_file` | `vfs_read_file` | `vfs_read_file` | `VfsRead` |

One admitted operation may produce multiple exposed tools. `KillShell` maps to
the continuation operation with its kill variant; the reverse lookup must retain
that variant. When continuation is disabled, omit all continuation exposures and
select the restricted run contract in both presentation and execution.

Resolution may select supported presentations and preserve existing documented
omissions, but cannot add grants. A required capability with no supported
implementation fails with a typed error before provider I/O. Resolving against a
different turn model does not imply support for arbitrary cross-provider replay
of existing provider-native context.

### Names, ordering, and tool choice

Validate the complete exposed namespace after built-ins, custom tools, MCP
expansion, and search helpers have been composed. Duplicate or ambiguous names
fail before the request is sent. Do not silently rename tools, overwrite a
binding, or accept an unadvertised alias from another provider surface.

Registry iteration order is not provider tool order. Preserve the existing
rendered order and placement of provider/MCP helpers using parity fixtures.
Otherwise sorting the new internal ids can change the prompt prefix despite
unchanged descriptions and schemas. Preserve Anthropic cache breakpoints and
other request additions associated with tool ordering.

`ToolChoice::Specific` targets the internal registration. Resolution translates
it to the designated primary exposure. This refactor adds no variant selector
to the tool-choice API. An unavailable choice fails before sending.
The engine validates admitted identity; the resolver validates presentation
availability. Public callers no longer need provider aliases to select a tool.

## Decision 4: Normalize Routing, Preserve Provider History

The response adapter uses the exact reverse lookup built for that request to
resolve each returned client tool call. This also applies to all continuations
within one LLM activity, including Anthropic `pause_turn`.

The engine receives the call id, admitted internal identity, argument/content
refs, and only the provider-neutral facts required for scheduling. Acceptance,
parallelism, workflow lookup, promise controls, and environment batch rules use
that identity. Replace existing checks for public spellings such as `await`
with the corresponding internal identity or admitted execution semantics.

Unknown or unadvertised names remain unavailable calls under the existing tool
error flow. They do not resolve through a global alias table. Malformed tool
arguments remain failures of that call, so one bad call does not discard valid
sibling calls or turn a tool error into an LLM transport failure.

### Use the same resolver for execution

Reuse the existing call records, original provider call, admitted registration,
and originating turn model to select the same argument adapter and result
renderer on execution. Carry the required facts on ordinary activity inputs.
The engine handles internal identities and opaque payload refs; it does not
interpret schemas or provider arguments.

Do not create a separate versioned call envelope or persist a second resolved
catalog. The model cannot choose its grant options or execution destination.
Both per-call and batch-unit execution use the admitted registration and original
turn model, including one-shot policy and the original exposed variant. Retries
and parked batches retain those facts rather than consulting current defaults.

For already-admitted Completions calls under provider-default settings, execution
also recognizes the former canonical `run_process`, `continue_process`,
`apply_patch`, and `vfs_apply_patch` bindings. This requires the matching admitted
logical identity and original exposed name and retains one-shot restrictions.
It is execution-only: request catalogs and response normalization never accept
these historical spellings as aliases for newly advertised tools. Provider-native
context replays the original call JSON even when the next request has a different
tool catalog.

Delete the default all-provider `runtime_catalog` reconstruction path and the
old built-in schema-document builders. A runtime lookup must use the shared
resolver and original call facts, rather than infer policy from a public name.

### Transcript and results

Keep the original provider tool call, exposed name, call id, and argument shape
in provider-native context. Separating dispatch identity must not rewrite past
`Bash` calls into `env.run_process` or re-render old calls using a new model's
surface. Result items pair with the original call ids and use the selected
renderer, preserving current visible text, handles, truncation, and errors.

The current `ObservedToolCall.tool_name` and transcript tool name cannot continue
serving as a single routing/display identity. Change the records and projections
explicitly. API views distinguish internal registration ids from historical
display names; the UI continues to show the name used in the actual exchange.

## Decision 5: Preserve Adjacent Execution Boundaries

MCP inventory remains runtime-owned and is not expanded into engine registry
entries. Its exposed names join the request-local namespace and normalize to the
admitted server identity plus remote tool identity. Search helper calls retain
their admitted server scope. Auth, allowlists, approval decisions, and fresh
credential resolution remain enforced on execution and re-dispatch.

Keep native MCP dispatch identical on per-call and mixed batch paths. Identity
normalization must not send injected calls into the built-in executor or convert
provider-hosted MCP calls into client effects. Preserve existing discovery,
search, tool-count, and name-length behavior while composing the final catalog.

VFS and environments retain separate identities, contexts, and permissions.
Environment selection remains a batch fact, and the selected environment id is
captured for execution as today. No filesystem overlay or implicit sync is
introduced. Workflow-backed tools continue through the generic workflow-tool
protocol, including their joined/explicit-promise and cancellation behavior.

## Replay and Deployment

This greenfield change does not migrate sessions whose built-ins were admitted
through the old materialized-definition path; recreate those sessions when
adopting it. The implementation retains no compatibility dispatch path.

Built-in definitions and adapters ship together with the runtime. There is no
per-tool definition version, historical implementation registry, or fallback to
the old blob-backed built-in path. Code changes can change the definitions used
by future activity executions; the parity fixtures make those changes explicit.

Event replay reduces recorded internal identities and payload refs without
loading definitions or executing provider codecs. Existing provider-native
transcript remains exact. Reissuing an activity uses the deployed implementation
with its original admitted options and turn model. This is the same deployment
contract as other runtime-owned argument adapters.

## Model-Facing Parity

The committed fixtures capture the pre-refactor provider/feature matrix.
The existing executable presentation is the baseline;
do not reconstruct it from older roadmap prose or copy newer upstream harness
definitions as part of this refactor.

The default acceptance is equality of the rendered tool contract and relevant
request fields, including tool order, descriptions, schema details, strictness,
provider options, cache markers, and helper placement. Compare strings exactly
and JSON structurally where object-key order has no meaning; preserve array
order. Freeze representative argument decoding and visible result behavior as
well: defaults, units, process handles, PTY/stdin, polling, kill, one-shot policy,
and error/truncation formatting.

Correcting a demonstrated mismatch between the advertised contract and execution
is allowed, including the one-shot reconstruction issue above. Record the
specific deviation and test it; do not use the refactor to rewrite prompts or
silently change the process substrate. Benchmark parity is behavioral evidence,
not a promise that stochastic scores will be numerically identical.

## Implementation Slices

### Slice 1 — Capture the parity baseline

- Add provider request and tool-result fixtures around the current builders,
  covering VFS, environment tools, one-shot/continuation, concurrency, web,
  workflow tools, and mixed MCP configurations.
- Retain the benchmark profile, model/reasoning configuration, and existing
  baseline artifacts for a representative rerun. Use the evaluation workflow
  described in [P149](p149-harbor-end-to-end-agent-evaluation.md).

### Slice 2 — Internal registry and definition sources

- Replace final-name keys and routing references in `engine` with internal ids;
  adapt tool choice, events, call facts, policy lookup, and workflow definitions.
- Split built-in registration from rendering in `tools::toolset` and all
  code-owned definition producers. Keep externally authored schema refs.
- Change gateway reconciliation to admit registrations without generating or
  storing built-in schema documents. Preserve revision guards and grant checks.

### Slice 3 — Resolution and call normalization

- Share binding resolution between the three LLM adapters and evaluation.
  Build native requests and reverse lookups using the effective turn model.
- Implement expansion, namespace validation, ordering, and specific-tool choice.
- Normalize returned identities, carry admitted call facts through execution,
  and preserve native transcript and continuation behavior.

### Slice 4 — Execution and consumers

- Replace default runtime catalog reconstruction with selected binding dispatch;
  use the same inputs on per-call and batch-unit paths, including re-dispatch.
- Update workflow validation, MCP normalization, promise/environment controls,
  API projections, CLI/web displays, and evaluation harness setup.
- Regenerate API and workflow contracts and TypeScript consumers after their
  source contracts change. Remove obsolete helpers and tests that enforce
  built-in schema storage; retain external-definition storage coverage.
- Update `README.md`, `docs/documentation/how-it-works/tools-and-controller-workflows.md`, and the relevant feature documentation
  when the implementation changes the architecture. Record implementation and
  verification progress here.

## Verification and Acceptance

- A built-in-only toolset can be admitted and rendered with no description or
  schema blobs in CAS. External definitions still load and validate their refs.
- Engine replay and checkpoint rehydration use internal identities and refs,
  with no schema resolution, registry I/O, or provider codec execution.
- One internal registry resolves to the expected three presentations, including
  run model overrides, explicit presentation overrides, and one-to-many tools.
- Golden request/result fixtures preserve the benchmarked surface. Repeated
  resolution preserves ordering and cache prefixes.
- Round trips execute the intended operation and variant. Cover `exec_command`,
  `Bash`, `BashOutput`, `KillShell`, VFS names, one-shot defaults, and invalid
  arguments beside valid calls.
- Cross-surface aliases, name collisions, missing choices, ungranted operations,
  and unsupported presentations fail clearly without dispatching another tool.
- A call survives worker reconstruction, retry, and batch park/resume using its
  original codec/options. Per-call and batch-unit routing remain equivalent.
- Provider-hosted web/MCP calls remain hosted; workflow and native MCP mixtures
  preserve existing approval, promise, cancellation, and completion behavior.
- Transcript replay preserves original exposed names, raw arguments, call ids,
  result pairing, and provider-native blocks after internal identity changes.
- Generated contracts and consumers are current. Scope Rust checks to `engine`,
  `tools`, `llm-runtime`, `temporal-workflow`, `temporal-server`, `api`,
  `api-projection`, and `eval`, then run the TypeScript checks for changed clients.
- Once the developer confirms the environment and credentials are safe, run the
  relevant ignored integration suites with their required serialization and a
  representative benchmark rerun. Record model/config parity and investigate
  material regressions; offline fixtures alone do not establish benchmark parity.

## Implementation Progress

- Built-in registrations use logical ids and compact settings. The engine retains
  its existing registry revision guard; there is no per-tool definition version.
- `tools::definitions` owns direct function/native definition resolution.
  Gateway admission writes no built-in descriptions or schemas to CAS, including
  system subagent/job declarations and MCP search helpers.
- All three LLM adapters resolve the request catalog, reject exposed-name
  collisions, translate specific tool choices, and normalize client calls before
  returning engine facts. Hosted calls remain outside client dispatch.
- Activity inputs carry the original built-in settings and turn model. The
  default all-provider catalog, schema bundles, tool documents, provider patch
  wrapper, and redundant dispatch metadata have been removed.
- Core scheduling, workflow binding lookup, promise controls, environment batch
  rules, and MCP routing use admitted identities. API call views carry the
  internal id alongside the historical displayed name.
- Eighteen complete provider request fixtures were captured from executable
  pre-refactor builders at commit `5707d076`, covering the three APIs across
  workspace, environment, one-shot, explicit canonical, web, and workflow
  configurations. Their regression test renders with an empty blob store.
- Verification passed: `cargo check --workspace --all-targets`; unit suites for
  `engine`, `tools`, `llm-runtime`, `temporal-server`, `temporal-workflow`, `api`,
  `api-projection`, `test-support`, `bots`, and `channels`; and the complete
  provider-request fixture comparison. Coverage includes engine event replay
  from a checkpoint, run model overrides, unknown aliases, collisions,
  serialized one-shot settings and continuation variants, hosted web tools,
  workflow calls, MCP approvals, and mixed batches.
- API and workflow contracts were regenerated, followed by the TypeScript
  consumers. `npm install` and the full `npm run check` passed. The generated-file
  checks compared regeneration against a temporary index of the new outputs;
  the user's Git index was untouched.
- Live subagent testing found two workflow-effect checks still comparing the
  exposed name with the registration identity. Emission and replay now validate
  the admitted id; the replay fixture uses different internal and exposed names.
- Live skill tests execute with an empty runtime catalog and assert both the
  internal VFS id and each provider's exposed name. Temporal provider tests
  exercise built-in timer and await calls across all three APIs and check their
  projected identities and successful results. The mixed native MCP test now
  checks every call's success, not just the final scripted response.
- Hosted-web live tests choose tools by their internal ids. DeepSeek live tests
  supply the required provider record, endpoint, and credentials through the
  current resolver; the redundant endpoint-only test and old client helper were
  removed.
- The generic fake model used by Temporal live suites now resolves built-ins
  instead of silently omitting them when selecting its next call. A regression
  test requires an actual tool round trip and distinct internal/exposed names.
  Session and profile registry assertions use the current admitted identities.
- Subagent cancellation waits for the parent's typed parked state rather than
  an exact count of runtime waiters, and checks the typed cancelled await
  outcome. The real bot test loads `.env` before checking provider credentials.

### Live verification

The user authorized the local services and credentials in `.env`. Temporal
suites run serially against the existing local PostgreSQL, MinIO, and Temporal
stack, with schema revision 15 current. Native MCP fixtures additionally require
`LIGHTSPEED_MCP_PRIVATE_NETWORKS=localhost,127.0.0.1,::1`; each fixture record
explicitly opts into private-network access.

All 74 selected live cases passed after fixes, including 20 real-LLM cases.
Counts below are unique cases; failed cases were rerun after correction.

| Suite or scenario | Passed |
| --- | ---: |
| Temporal sessions, including real built-in calls on all three APIs | 12 |
| Temporal run control, retries, parallel batches, and long drive sequences | 9 |
| Generic workflow-tool plugins | 14 |
| Joined/detached subagents, limits, inheritance, deadlines, and cancellation | 6 |
| Native MCP discovery, approvals, configuration, and mixed await batches | 5 |
| Profiles | 2 |
| Environment lifecycle and real envd registration/process routes | 3 |
| Bots, including a real-model workflow-tool delivery | 6 |
| Channels | 2 |
| OpenAI Responses and Anthropic skill/VFS engine loops | 2 |
| OpenAI/Anthropic function round trips, parallel calls, and DeepSeek reasoning | 6 |
| OpenAI/Anthropic hosted web tools | 3 |
| OpenAI/Anthropic hosted remote MCP | 2 |
| Anthropic and OpenAI Completions tool-round-trip prompt caching | 2 |

Provider-runtime tests used their defaults: `gpt-5.5`, `claude-opus-5`, and
`deepseek-v4-pro`. The final offline reruns passed the engine (209), LLM runtime
(141), Temporal server (306), Temporal workflow (110), and fake-loop (2) tests,
plus all 18 complete provider-request fixtures. Workspace/all-target checks and
format/diff checks passed; public wire types did not change during live fixes.

Complete provider-request equality establishes the advertised contract; these
live integration tests do not establish stochastic benchmark-score parity.
Full benchmark runs and the unrelated production-budget LLM timeout suite were
not included.

### Full workspace verification

The complete Rust verification pass also succeeded on 2026-09-05:

- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets --all-features --locked`
- `cargo build --workspace --locked`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked --no-fail-fast`: 1,753 passed, zero failed,
  and 192 explicitly ignored tests across unit, integration, and doc tests.
- CI's ignored migration-ledger test passed separately against an isolated
  PostgreSQL schema; release metadata verification passed as well.

Four Clippy findings were fixed by removing redundant iterator conversions, an
unnecessary test clone, and an obsolete single-case test loop. The affected test
was rerun after simplification. Final builds and checks emitted no warnings.
The ignored-test count is the standard workspace run's result; the selected
live integrations above were exercised separately.

### Frontend verification

The session/profile editor now explains that specific tool choices use registry
IDs, with builtin examples. The transcript reducer retains the optional admitted
ID separately from the original model name, including when context and batch
events arrive in either order. Rendering continues to use the recorded name;
unavailable aliases remain unbound.

Demo call fixtures carry explicit registry IDs through the same event API used
by the live UI. Four duplicate fixture helpers were removed. Regression coverage
checks provider-independent configuration, streamed and replayed transcripts,
tool-name rendering, and demo route output.

The full `npm run check` passed after these changes: generated client and
Configurator consistency, all TypeScript typechecks, 262 tests (143 frontend
tests), and production builds of the client, Configurator, live UI, and demo.
Generation was checked against a temporary index of the intended artifacts,
without changing the user's Git index. Both UI builds retain the pre-existing
Vite advisory about chunks larger than 500 kB; no new build warnings appeared.

### Public workflow input boundary

The public workflow input's built-in variant was removed after checking every
producer. Only internal system bindings need it: subagent and environment-job
declarations are built directly as engine types. External workflow declarations
always supply their own function definitions, and session management projection
already excludes system bindings.

The API input and gateway/projection conversion branches now accept only
functions. Bot/channel test branches introduced solely for the extra variant
were removed. Engine built-in definitions, internal workflow validation, and
read-only tool inventory keep their built-in support. Regression coverage rejects
built-ins in both JSON deserialization and the public schema, and checks the
management projection with both external functions and internal built-ins.

After removal, the API, projection, Temporal server, tools, bots, and channels
suites passed: 818 tests, zero failures, and 66 explicitly ignored live tests.
The API contract and TypeScript client were regenerated, and the complete
`npm run check` passed again with 262 tests and both UI builds.
Workspace Clippy over all targets with `-D warnings`, formatting, and diff checks
also passed.

## Out of Scope

- New tool capabilities, different model prompts, new provider presentations,
  or changing process/environment semantics to imitate another harness.
- A plugin loader, versioned tool registry, new call-envelope storage layer,
  remote schema registry, or a second durable expanded catalog.
- Eager normalization of all provider requests into a common wire format.
