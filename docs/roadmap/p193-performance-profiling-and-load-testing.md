# Performance profiling and load testing

**Status:** First milestone report published, 2026-10-19. The HTTP simulator,
public-API driver, initial local measurements and validation are complete. Issue
#121 contains the findings and raw-data links; PR #122 is open for human review.

## Outcome

Find the current throughput limits and avoidable costs in Lightspeed, explain
them with profiles and measurements, and verify improvements with repeatable
workloads. Produce a small performance test setup that remains useful as the
system changes.

Lightspeed has been exercised with roughly 20 simultaneous benchmark sessions.
An upcoming bank deployment may grow to 100 or more concurrent users and
200–500 concurrent sessions. These numbers motivate the work; reproducing that
many users or sessions is not a prerequisite for the investigation. Start with
a small, fixed deployment and enough independent sessions to generate pressure.
Increase concurrency only when it is the dimension being tested or when more
parallel work is needed to keep the deployment busy.

The immediate deliverable is a measured account of where the system spends
time, where work queues, and which changes improve capacity. A capacity claim
for the bank requires later validation against its workload and deployment.

## First handoff: initial findings and data

The first implementation task is limited to the simulator, load driver, minimum
instrumentation and initial measurements of rapid agent loops, event readers
and large histories. Use text-only turns and admitted VFS/blob tools as needed
to stress the hosted session, API, storage and Temporal paths. Keep the deployment
and resources fixed while collecting the first baseline and saturation curves.

Stop before environment profiling: no envd, environment-gateway, Incus/VM,
environment-backed process/tool or file-transfer capacity experiments. Independent
code-interpreter profiling, broader fan-out/mixed-load campaigns, extensive
recovery/soak tests and bank-scale capacity qualification are follow-up work.
They remain in the roadmap below to inform later planning, not as instructions
to continue this first task.

Publish the initial report as a comment on the GitHub issue that owns this task.
The issue is the review and planning destination; a local file or PR description
alone does not complete delivery. Include:

- The published implementation revision, test results, deployment/resource
  configuration, scenario definitions and exact reproduction commands.
- Offered and completed throughput, p50/p95/p99 latency, errors/unfinished work,
  queue behavior and available CPU/memory/database/Temporal observations for
  each load step, including drain behavior after arrivals stop.
- Profiles or query evidence supporting each finding, generator/simulator
  headroom, and accessible links to retained data and larger artifacts. Include
  compact tables directly in the issue comment; do not rely on local paths.
- Observed bottlenecks, remaining hypotheses, measurement gaps and a ranked
  proposal for the next experiments or optimizations. Do not present missing
  measurements as zero cost or claim bank capacity from these initial results.

Make only changes needed to enable and validate the measurements during this
first task. Report candidate optimizations instead of expanding into a tuning
campaign. Publish the implementation for review, do not merge, and stop after
posting the report so the next scope can be chosen from evidence. If a scenario
is blocked, publish the available findings and the concrete blocker rather than
silently omitting it or moving on to environment work.

## Starting point

Relevant existing boundaries and candidate costs:

- [Hosted session activities](../../crates/temporal-runtime/src/worker/activities/llm.rs)
  call `CoreAgentLlm`; the normal implementation in
  [llm-runtime](../../crates/llm-runtime/src/executor.rs) materializes requests and
  processes results through provider-native
  [llm-clients](../../crates/llm-clients/src/lib.rs).
- [Provider endpoint configuration](../../crates/llm-runtime/src/provider_keys.rs)
  and [HTTP endpoint overrides](../../crates/llm-clients/src/transport/http.rs)
  already allow requests to reach a test server. The
  [endpoint-override tests](../../crates/llm-clients/tests/openai_endpoint_override.rs)
  demonstrate real client requests against local HTTP responses.
- The existing [FakeLlm](../../crates/temporal-runtime/src/worker/fake.rs) supports
  delays, tool rounds, parallel calls and failures for correctness tests. It
  replaces the higher-level LLM implementation and bypasses provider request
  materialization and HTTP. Keep its existing test role; do not extend it into
  a second load-testing simulator.
- The Rust runtime and Platform currently have no application Prometheus
  endpoint. [Runtime workers](../../crates/temporal-runtime/src/worker/mod.rs)
  construct SDK runtime and worker options with defaults at this boundary.
  [PostgreSQL pool construction](../../crates/temporal-runtime/src/config.rs)
  likewise uses default pool options.
- [Forward event reads](../../crates/temporal-runtime/src/gateway/service/mod.rs)
  implement long-polling by checking storage at intervals capped at 250 ms;
  with current defaults, a continuously waiting reader makes about four
  storage checks per second. Reader count therefore creates load independently
  of agent activity. A storage check need not equal one SQL statement.
- Gateway reads can reconstruct session state from checkpoints and subsequent
  events. History size, cache state and checkpoint position are dimensions to
  measure, rather than assuming every session has the cost of a fresh one.
- The `code` worker defaults to four simultaneous interpreters per process.
  The environment gateway holds live connections in process and requires one
  active process per deployment. Other worker roles can have replicas after
  roles are split; replicating the default all-role process is not the scaling
  recipe.

These are hypotheses and configuration facts, not measured bottlenecks.
Existing [testing guidance](../documentation/development/testing-and-evaluation.md)
and [operations guidance](../documentation/deployment/operations.md) remain the
starting points for running and observing the system.

## One LLM simulator at the HTTP boundary

Add a small workspace binary crate, `crates/llm-simulator`, running as a separate
process alongside the deployment under test:

```text
Load driver → public API → Temporal/session execution
                             → llm-runtime → llm-clients → llm-simulator
```

Configure Lightspeed to use the simulator through its normal model-provider
endpoint configuration. Context assembly, credential resolution, request
serialization, HTTP transport, response parsing, CAS operations, tool execution
and workflow scheduling continue through their normal implementation.

The simulator owns provider-compatible HTTP responses and shared scenario
behavior. It has no dependency on the harness, Temporal, Lightspeed stores or
workflow internals. Reuse provider-native types from `llm-clients` where useful;
production binaries do not depend on the simulator. Do not add simulation
branches to the harness, LLM activities or provider adapters.

Start with the OpenAI-compatible Chat Completions API (`openai:completions`),
serving `POST /v1/chat/completions` through the existing request-time endpoint
override. Exercise native messages, assistant tool calls, tool-result messages,
finish reasons and usage through the real Chat Completions client and adapter.
Implement the operations actually used by the initial scenarios. Responses and
other provider formats can be added later as protocol handlers within this same
simulator, sharing scenario logic. There is no second in-process model simulation
for this investigation and no requirement to emulate every provider API before
collecting useful results.

Scenario configuration controls:

- Response delay, seeded jitter, and deliberate stalls.
- Valid response payload size and reported usage, including cached-token usage
  when exercising accounting paths. Reported usage is synthetic; it does not
  measure model inference or actual provider caching.
- Tool-call sequences, calls per turn, rounds before a final answer, and
  scripts that invoke the tools admitted in the real request.
- Provider failures such as 429/503, `Retry-After`, and context-length errors.
- Summary completions when adding compaction scenarios: the Chat Completions
  adapter requests a summary through the same completion endpoint. A separate
  native compaction endpoint is not needed for the initial protocol. Unsupported
  operations fail explicitly so missing simulator coverage cannot appear as a
  successful measurement.

Progress through a tool sequence from the request's native conversation and
tool results, with stable scenario selection through existing supported request
fields, such as the selected model name. Any state needed for failure schedules
must be scoped to the scenario and conversation/request. Concurrent sessions
and retries must not consume a single global step counter. Keep request
correlation inside the test protocol and existing fields; no benchmark-only
public API or durable session fields are needed.

The simulator executes no tools itself. It returns valid calls; Lightspeed
executes them against the configured VFS, code worker, environment or test MCP
service. Required tools and grants are part of scenario setup, and a missing
capability fails the scenario clearly.

Honor normal endpoint validation: local HTTP can use loopback; remote simulator
deployment uses HTTPS where required. Do not weaken production endpoint checks.
The simulator reports request rate, in-flight requests, errors and response
latency so its own saturation is visible. Keep it independently resourced when
needed, and check its headroom before attributing a limit to Lightspeed.

## Load driver and reproducible runs

Put a lightweight TypeScript driver and checked-in scenario definitions under
`scripts/perf/`, reusing the generated TypeScript API client where applicable.
The driver creates or prepares sessions, submits work, follows events, checks
outcomes and records results. It generates user/API traffic; all LLM simulation
remains in `llm-simulator`.

Begin with the public runtime HTTP API. Add a path through Platform using real
test-user authentication, membership and visibility checks, plus browser-style
event tails and history reads. Direct runtime access alone cannot establish
the cost of the Platform path. Protocol-level clients can create most of this
pressure; large fleets of automated browsers are not needed for the first
investigation.

Support both fixed-concurrency loops and independent arrival-rate scheduling.
In the latter, slow completions must not silently reduce the configured arrival
rate. Record scheduled, submitted, rejected, completed, failed and unfinished
work, generator dispatch lag and any work the generator could not submit.
Bound the experiment and report overload rather than allowing unbounded local
queues. Distinguish generator limits from service limits. See
[open and closed workload models](https://grafana.com/docs/k6/latest/using-k6/scenarios/concepts/open-vs-closed/).

Use multiple independent sessions so one session's serialized execution is not
mistaken for a deployment-wide ceiling. Record active runs, waiting activities,
queued work and event readers separately; an open session is not a unit of
throughput.

Each run records the Git revision and dirty state, build profile, machine and
container resource limits, role placement and replica counts, database/Temporal/
object-store versions and topology, pool/worker/cache settings, scenario
configuration, seed, dataset/history size, warm-up and measurement durations,
and generator/simulator placement. Preserve the effective settings without
credentials. Store raw results and profiles in a configurable artifact directory
outside tracked source; commit concise findings and reproducible scenarios.

Use an isolated test deployment with unique run/session identifiers, task queues
or namespaces as appropriate, and isolated database/object data. Follow `./dev.sh`
for supported local infrastructure setup and the normal migration/startup path.
Release builds are required for capacity measurements. The generator should run
outside the service's constrained CPU/memory allocation. A single-host setup is
useful for discovery if shared host contention is recorded; validate important
findings on the intended deployment topology before extrapolating capacity.

## Measurement and profiling

Add enough instrumentation to attribute a slowdown before increasing load. Start
with a small, explicit set rather than making a general observability redesign
a prerequisite for the first result.

| Layer | Measurements |
| --- | --- |
| API and Platform | Requests and errors by method/route, p50/p95/p99 latency, in-flight requests and waiting event readers. |
| Agent execution | Runs, model turns, tool calls and event appends per second; admission, first visible progress and terminal completion latency. |
| Temporal | Workflow/activity schedule-to-start and execution latency, occupied/available slots where supported, backlog/age, retries, timeouts, cache evictions and replay cost. |
| PostgreSQL | Pool acquisition wait, connections, query calls and total execution time, lock waits, I/O, WAL and storage growth. |
| CAS and context | Bytes and operation latency, cache hits/misses, request materialization and response projection/reconstruction cost. |
| Processes | CPU, resident memory, allocation behavior, disk/network traffic and Node event-loop delay for Platform. |
| Test infrastructure | Generator dispatch lag and achieved rate; simulator latency, in-flight requests and resource headroom. |

Use bounded metric labels such as role, method and outcome. Put session/run
identifiers in correlated traces or logs rather than creating a metric series
per session. Measure service processing separately from external-call delay and
queueing. Do not treat a successful admission response as a completed run.

Enable Temporal SDK metrics through the supported SDK runtime configuration.
Collect service-side Temporal observations too; worker metrics alone do not
explain server persistence or throttling. Track pool wait separately from query
execution so increasing worker replicas does not hide connection contention.
Use [Temporal's tuning metrics](https://docs.temporal.io/develop/worker-performance/metrics)
and [workflow-cache guidance](https://docs.temporal.io/develop/worker-performance/workflow-cache),
applying settings supported by the Rust SDK version in the checkout.

Use CPU sampling/flamegraphs and allocation or heap profiles at selected load
points. Use PostgreSQL
[`pg_stat_statements`](https://www.postgresql.org/docs/current/pgstatstatements.html)
to find queries with high total cost, including cheap queries executed very
frequently. Inspect plans for the queries implicated by the measurements.
Keep full provider-payload debug dumps disabled in baseline runs because they
add storage work; record diagnostic logging and profiling overhead when enabled.

## Workload matrix

Keep the first sweep focused on one variable at a time. Add mixed workloads
after the individual costs are understood.

| Scenario | Stress variables | Main questions |
| --- | --- | --- |
| Rapid agent loops | Minimal model delay, turns/second, tool rounds and event volume. | What limits orchestration and persistence throughput? |
| Slow external calls | Model/tool delay and concurrent in-flight calls. | What consumes activity capacity or retained memory while waiting? |
| Tool and sub-agent fan-out | Calls/children per turn, result sizes and durations. | How does fan-out amplify scheduling/storage cost and affect unrelated runs? |
| Large and mature sessions | Context bytes, historical event count, checkpoint position and tool-result size. | How do assembly, reconstruction, projection and compaction scale? |
| Interactive reads | Idle/active event readers, history pagination, lists and reconnect bursts. | How much read amplification exists, including through Platform? |
| Code and environments | Concurrent interpreters, real process/tool calls and file transfers. | Where do code admission and the single environment gateway limit progress? |
| Failures and recovery | Scripted provider failures/stalls, cancellation and a worker restart. | Do retries amplify load, controls remain responsive, and queued work recover? |

For histories and payloads, use several controlled sizes and record them exactly;
for example, 100/1,000/10,000 events where valid and practical. Prepare histories
through supported operations and distinguish fixture preparation from measured
work. Include warm caches, cold workers and the first request after a restart.
Reader count and slow-call concurrency may require more concurrent clients than
the rapid-loop scenario; low session count is a starting convenience, not a cap.

Measure interactive reads, cancellation and a small independent run while a
heavy scenario is active. Throughput gains must not conceal starvation or loss
of control responsiveness.

## Experiment procedure

1. Fix the deployment revision, resources and role topology. Establish an idle
   baseline and a low-load baseline with warm-up separated from measurement.
2. Increase one variable in steps, holding each long enough for throughput,
   latency and queue depth to settle. Record offered and completed work together.
3. Locate the point where completed throughput flattens, p95/p99 latency rises
   sharply, or backlog grows continuously. Sustained queue growth is overload,
   even when admission still succeeds.
4. Profile below and around that point. Attribute time to CPU work, allocation,
   pool/query/lock waits, Temporal scheduling, CAS I/O or external latency. Repeat
   enough to distinguish a stable effect from run-to-run variation.
5. Reduce or stop arrivals and verify that queued work drains. Observe memory
   after warm-up and drain; a retained cache is different from continuing growth.
   Account for every admitted run through a terminal result or an explicit
   unresolved failure, and check event continuity and expected tool outcomes.
6. Change one implementation detail or setting and rerun the same workload.
   Compare complete throughput/latency/resource curves, not only the best result.
7. Add a second relevant worker replica with shared-service capacity held fixed.
   Record both additional resources and throughput gained. If scaling is poor,
   investigate the new bottleneck before adding more replicas.

Separate roles before replica experiments, retaining one active environment
gateway. Record aggregate database connections and per-process cache memory.
After the initial fixes, run a longer mixed workload to expose gradual memory,
storage or queue growth, followed by targeted restart/recovery checks and
representative concurrency validation. Real-provider validation is a separate,
explicitly selected experiment; it is not required to locate internal limits.

## Delivery and acceptance

- [x] Add `llm-simulator` with the Chat Completions protocol, scenario validation,
  deterministic tool sequences, delays, payload sizes and scripted failures.
- [x] Add `scripts/perf/` driver, scenario fixtures, effective-configuration
  capture and machine-readable results.
- [x] Add the minimum runtime/Platform, pool and Temporal instrumentation needed
  to distinguish queueing from execution and external delay.
- [x] Establish initial curves and profiles for rapid loops, event readers and
  large histories on one documented, fixed deployment.
- [x] Publish the first findings/data report on the owning GitHub issue and stop
  for review and selection of follow-up work, before environment profiling.
- [ ] Record measured findings, prioritize fixes, and rerun the same experiments
  for each optimization or capacity-setting change.
- [ ] Extend to slow calls, fan-out, code/environment paths, mixed load and
  recovery where relevant to the intended deployment.
- [ ] Validate the resulting configuration at representative bank concurrency
  and workload, with explicit service objectives and capacity headroom.

The first milestone ends with the simulator, driver, instrumentation, three
initial baseline experiments and the issue report described above. Subsequent
checklist items require a new follow-up scope; they are not part of the initial
handoff. Completing every workload family is not a prerequisite for reviewing
the first observed bottlenecks.

Each finding records its reproducer, environment, offered/achieved load, latency
distribution, resource and queue evidence, suspected cause, and any before/after
comparison. Classify whether it needs a code optimization, a configuration
change, shared-service capacity or additional workers. Keep unmeasured
hypotheses separate from established findings and state limits on extrapolation.

Simulator tests cover valid native responses through the real client/adapter,
concurrent scenario isolation, bounded state, retry/failure scheduling and tool
round completion. Driver tests cover scheduling/accounting, deadlines and failed
or unfinished runs; no silent success when infrastructure or tools are missing.
External integration suites are explicitly selected and remain isolated from
ordinary unit tests. Existing ignored Temporal suites still run serially under
the repository's live-test rules. No test setup may make unrequested billable
model calls.

At proposal creation, no simulator, driver, instrumentation or load experiment
has been implemented or run. Add implementation progress and measured results
here as work completes.


## Initial implementation progress — 2026-10-19

The local implementation adds `crates/llm-simulator` and `scripts/perf/`. The
simulator serves native Chat Completions through normal endpoint configuration;
provider adapters, CAS and Temporal remain on the execution path. Scenarios
cover deterministic text sizes, usage, delays/jitter, conversation-derived tool
rounds and persistent 400/429/503 failures. Tests exercise native client tool
round trips and concurrent conversations. The explicitly selected
`simulator_local` integration test exercises the real runtime adapter.

The driver uses the generated TypeScript API client, independent arrivals,
bounded in-flight work, event following, deadlines, explicit dropped/failed/
unfinished counts and drain accounting. Fixtures distinguish preparation and
warm-up from measured arrivals. The VM sweep includes rapid loops, idle and
active readers, and histories prepared through ordinary runs. It uses only
synthetic loopback traffic and release binaries, with gateway and session roles;
no environment capacity tests or billable model calls are involved.

Minimum instrumentation consists of optional SDK Prometheus export through
`LIGHTSPEED_TEMPORAL_METRICS_BIND`, SQLx pool acquisition timing at debug level
(`RUST_LOG=info,sqlx::pool::acquire=debug`), query-statistics snapshots, process
samples and generator/simulator counters. Debug acquisition logging is reserved
for a separately identified diagnostic run. Raw data are retained outside
source under `/tmp/issue-121-perf` and published as the data-only prerelease
linked from the issue #121 report comment. PR #122 carries implementation for
human review.

Initial rapid-loop measurements show queueing at 10 offered runs/second on
this development topology: 149 of 300 scheduled runs were submitted under a
64-session bound, all submitted runs completed, and drain took 8.4 seconds.
Median terminal latency rose from roughly 0.60 seconds at 2 offered runs/second
to 15.05 seconds. This is a limit of the measured local topology, not a capacity
claim for a production deployment. Full attribution, all three scenario families,
validation and coverage gaps are documented in the issue #121 report and linked
archive. Follow-up optimization and environment work remain out of scope.


The completed sweep also found a gateway file-descriptor limit: 512 idle reads
per second completed cleanly, while 2,048 offered reads per second produced
transport errors and gateway `EMFILE` accept errors with a soft descriptor
limit of 1,024. This limit was recorded without tuning it. Mature fixtures began
with 333 events and 528 KiB of input/output text per session; their admitted runs
completed at all three load steps, with excess arrivals explicitly dropped.
A rapid-loop repeat differed by about two percent in including-drain throughput.
Direct simulator tests completed 1,000 small requests per second and 100 large
requests per second, above the measured runtime rate.

The initial curves and query profiles are collected, including raw timestamps,
SDK metrics, resource samples, query-statistics deltas and drain results. Hardware
CPU/allocation profiles, Platform authentication, complete CAS/cache counters
and production Temporal persistence remain coverage gaps. The reader ceiling is
a measured configuration limit; the source of Temporal queueing still needs
service-side attribution. No optimizations were attempted. Final check results
and the accessible raw-data links are published in the issue #121 report comment.
PR #122 is open for human review; this planning issue remains open for follow-up.


Validation passed: four simulator tests, six native endpoint/client tests, the
explicit local adapter/CAS test, nine runtime configuration tests, seven driver
scheduling/accounting tests, scoped TypeScript checking, the exact workspace
Clippy gate with all targets and warnings denied, documentation checks and local
roadmap-link validation. No credentialed provider suite or existing ignored
Temporal suite was executed. Platform instrumentation remains an explicit gap
in the minimum first-pass instrumentation, rather than measured coverage.

The SDK metrics endpoint became unreachable during the descriptor-exhaustion
step and recovered only after runtime restart. Later missing SDK observations
are marked unavailable in the report. A separate pool-timing diagnostic at
512 reads per second measured 0.31 ms p99 acquisition wait; it does not explain
the higher-load descriptor failures or establish a production pool size.

### PostgreSQL Temporal follow-up

The same issue and PR workstream now includes an isolated PostgreSQL-backed
Temporal setup under `scripts/perf/temporal-postgres/`. History and visibility
use separate databases on a dedicated PostgreSQL container; Lightspeed uses a
new database on the existing local PostgreSQL service. Named volumes and prior
SQLite results are preserved. Images are pinned by digest, and the setup records
schema verification, effective configuration, resource placement, per-database
query snapshots, server/SDK metrics and PostgreSQL wait samples. Optional pprof
collection is reserved for separately labeled diagnostic repeats.

The available official server image reports Temporal 1.29.3, while the initial
SQLite dev server reported 1.32.0. Consequently this is a topology comparison,
not a controlled attribution of performance differences to the persistence
engine alone. The version difference and all unavailable measurements must
remain explicit in the follow-up report. No production behavior or optimization
is changed. This follow-up is carried on existing PR #122, and its separate
results report is published on issue #121.

The follow-up completed nineteen measured steps, including clean repeats and a
separately profiled rapid-loop step. All 3,921 submitted agent runs completed.
At 40 offered rapid-loop runs per second, including-drain throughput was 16.30
and 16.32 runs per second in two unprofiled trials, compared with 3.88 in the
initial SQLite topology. Mature histories completed 336 runs at that offered
rate with the unchanged sixteen-session bound. The reader descriptor ceiling
recurred: 512 reads per second completed cleanly, while 2,048 produced transport
failures and gateway descriptor-exhaustion errors; no unfinished work remained
after drain. Temporal CPU/allocation profiles and database-specific query
statistics are retained with the follow-up report under
`/workspace/artifacts/issue-121-temporal-postgres`. The evidence identifies
workflow queueing and persistence/RPC volume, reader polling/descriptor costs,
and transient server allocations as investigation targets, not completed
optimizations. Version matching, Platform-authenticated reads and Rust CPU/
allocation attribution remain gaps. The updated findings, reproduction commands
and accessible raw-data links are published in a separate follow-up comment on
issue #121. No tuning was performed; await explicit follow-up before further work.

### Focused PostgreSQL baseline improvements

The explicitly reopened follow-up uses the latest PostgreSQL deployment as its
working baseline. Controlled local A/B arms preserve the same release runtime,
workloads, worker/pool settings and VM budget while comparing Temporal's
supported `postgres12` and `postgres12_pgx` drivers. Both history and visibility
use the selected driver. Two unprofiled saturation pairs completed 16.42/19.09
and 15.29/19.43 runs per second respectively, with every submitted run completing.
Persisted sessions resumed successfully across both driver transitions. The
performance setup now defaults to `postgres12_pgx`, retaining an explicit
control override and all prior results.

The local performance launcher also defaults to an 8,192 soft descriptor limit.
At 2,048 offered idle reads per second, this removed the submitted-request
failures seen with the 1,024 limit. Generator concurrency drops and scheduling
lag remain visible, so this does not establish unrestricted reader capacity.
A small PostgreSQL store fast-path prototype preserved the tenant-visible session
lookup while skipping lineage resolution for caught-up readers. Two comparisons
reduced SQL calls from twelve to seven per read and runtime CPU by 34–37% at
512 reads per second. Selected live tests passed for tenant isolation, forks,
subsequent appends, range fencing and deleted history. However, three rapid-loop
comparisons, including reversed order, showed lower throughput and longer drain.
The prototype and tests are retained as a separate evidence patch, not included
in the proposed production source changes. Its mixed-workload tradeoff remains
unresolved; no reader fast path is adopted in this follow-up.

The follow-up report, all-step tables and raw A/B evidence are published in a
separate update comment on issue #121 and the data-only prerelease
`perf-issue-121-low-hanging-2026-10-19`. Temporal profiles still show substantial
history-task and metrics allocations. Larger workflow changes, reader
notification mechanisms, shared-service sizing and worker scaling remain
proposed experiments, not changes made in this follow-up. No environment
profiling, production traffic, billable calls or additional workstream was
introduced. PR #122 remains open for human review; no merge or further tuning
is authorized without another explicit instruction.

### Session workflow boundary investigation

The next explicitly reopened investigation retains the PostgreSQL/pgx baseline
and adds offline Temporal-history attribution under `scripts/perf/temporal-postgres/`.
Four fresh load steps completed 1,088 admitted agent runs and 15,360 idle reads
without failures or unfinished work. Fifteen retained session histories yielded
225 complete measured runs. Each text run used eleven activities: eight event
appends, tool preparation, runtime projection refresh and generation. At rapid
saturation the mean sampled cycle was 2.46 seconds, including 0.86 seconds in
activity queues and 1.25 seconds waiting for ten intervening workflow tasks.
Server-observed execution intervals include transport/persistence and are not
CPU measurements. Separate source inspection identifies the append boundaries
as Lightspeed choices; Temporal durability does not require one activity for
every reducer transition.

No workflow/runtime optimization is adopted. Combining the pre-generation
append prefix is a candidate for a future approval-gated design experiment,
with replay, cancellation, expected-head, public event visibility and retry
semantics checked first. Reader polling, larger mature-history payloads and the
previously profiled Temporal allocations remain measured costs or explicitly
labeled follow-up hypotheses. The offline workflow-history attribution analyzer and deterministic tests are
included in the same PR #122 workstream; the report and raw evidence accompany
the issue #121 update. PR #122 remains the sole implementation workstream and
is unmerged. The prior publication-status edit is preserved. No workflow/runtime
optimization is proposed until the user reviews the boundary tradeoffs.

### Focused bookkeeping A/B experiment

The explicitly reopened experiment combines only `turn.planned` and
`turn.generation_requested` in one expected-head append, guarded by a Temporal
patch marker. Both public events retain their sequence positions but become
visible atomically. The private speculative reducer does not mutate live state
or execute effects. This remains an uncommitted experiment for owner review,
not an adopted workflow optimization; no broader batching is implemented.

Eight PostgreSQL/pgx load steps completed 3,652 admitted runs without failures
or unfinished work. Two rapid-loop control/variant pairs increased completion
throughput during arrivals by about 4%, but throughput including drain changed
by -2.2% and +3.5%. One mature-history pair improved throughput including drain
by 6.3% and median latency by 15.1%; it is not a replicated capacity claim.
Retained histories confirm eleven to ten activities and eight to seven appends
per simple run; Temporal history reads fall from twelve to eleven and execution
updates from 48 to 44. Queueing still dominates sampled cycles. Per-task queue
latency does not improve, so the evidence supports reduced serial work, not a
claim that persistence or worker matching has been fixed.

Workflow unit tests, cancellation/event visibility, steering, queued-run checks,
forty retained-history replays and the exact workspace Clippy gate pass. The
queued-run fixture now explicitly requests zero tool rounds: its existing
one-generation assertion failed in both control and variant because default
blob tools allowed an extra tool turn. Initial failures and corrected results
are retained. The local report and raw evidence are prepared for Engineer owner
publication on issue #121 in the existing PR #122 workstream; nothing is
committed, pushed, commented or merged by this experiment. Earlier publication
status and all previous evidence remain intact. The next recommended diagnostic
is a bounded worker-polling/matching comparison, not a broader workflow redesign.
