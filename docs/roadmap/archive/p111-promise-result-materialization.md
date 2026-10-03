# P111: Promise Result Materialization And Readable Environment Job Output

**Status**

- Completed 2026-08-01.
- Greenfield breaking change. Do not preserve the current synthetic-user-message
  transcript shape, add a compatibility branch, or version running Temporal
  histories solely for this change.
- Replaces the model-visible result behavior of explicit P92/P94 `await` while
  preserving Promise lifecycle, ownership, cancellation, detach, timeout, and
  wake semantics.
- Adopts the readable-job-output work from
  [Environment Execution Usability](later/pNNN-environment-job-model-usability.md).
  That document remains the home of the separate joined-job-start and
  environment-variable proposals.
- Builds on [P92](p92-unified-suspension.md),
  [P94](p94-engine-native-suspension.md),
  [P100b](p100b-workflow-backed-tools.md), and
  [P106](p106-joined-workflow-tools.md).

## Problem

Before P111, an explicit `await` completed with two different model-visible
shapes:

1. the `await` call receives a short tool result such as `await resolved with
   outcome terminal`; and
2. every resolved Promise payload or failure detail is appended immediately
   afterward as a `Message { role: User }` context entry.

This is not merely an API projection or UI presentation. The provider adapters
materialize those entries as real user messages, so the durable model
transcript claims that Promise results came from the user.

The behavior originated in the former Fleet-specific `agent_wait`: child-run
final output was appended as a user message after a compact wait summary. P92
preserved that representation when it unified run, environment-job, timer, and
workflow completion behind generic Promises. A subagent-specific transcript
choice therefore became the behavior of every Promise source.

The result is wrong at both abstraction boundaries:

- `await` is the model action that requested the completion snapshot, so the
  snapshot belongs to its tool result;
- job, workflow, and subagent output is produced by a tool or asynchronous
  source, not by the user;
- clients render a successful `await` card followed by an unexplained user
  bubble containing raw output;
- scripted consumers must scan both the tool result and the newest user
  message to reconstruct one logical result; and
- Promise payloads containing environment-job transport DTOs expose Base64
  byte chunks rather than readable output.

Environment jobs make the problem especially visible. The environment protocol
correctly represents arbitrary process bytes as Base64 `ByteChunk` values, but
the environment-job workflow currently serializes the raw `JobReadResult` as
the Promise payload. The Base64 transport representation consequently crosses
the model-facing boundary unchanged.

## Decision

An explicit `await` produces exactly one ordinary model-visible tool result for
the `await` call. That result is a structured, total snapshot of every Promise
named by the call, in requested order.

Conceptually:

```json
{
  "outcome": "terminal",
  "results": [
    {
      "promise_id": "wtp:sha256:...",
      "status": "resolved",
      "output": {
        "summary": {
          "jobId": "demo_job_001",
          "status": "succeeded",
          "exitCode": 0
        },
        "output": [
          {
            "stream": "stdout",
            "text": "Job started\nJob finished\n"
          }
        ],
        "outputNextSeq": 5
      }
    },
    {
      "promise_id": "run:...",
      "status": "resolved",
      "output": "The delegated agent's final response"
    },
    {
      "promise_id": "timer:...",
      "status": "pending"
    }
  ]
}
```

The result keeps P92's existing total semantics:

- `all` and `any` report every requested Promise, including those still
  pending;
- timeout and mailbox wakes are successful `await` completions with a partial
  snapshot;
- a failed Promise is one result with `status: "failed"` and an `error`
  value; it does not turn the `await` call itself into a tool error;
- cancelled Promises report `status: "cancelled"`;
- invalid arguments, unknown Promise ids, and foreign ownership remain errors
  of the `await` tool call itself; and
- pending Promises remain re-awaitable and terminal Promises may be observed
  again according to the existing Promise contract.

There is no synthetic summary plus payload split and no Promise-derived user
message.

## One Promise, One Root Value

A Promise continues to resolve to zero or one root payload reference:

```text
Resolved { payload_ref: Option<BlobRef> }
Failed   { error_ref: Option<BlobRef> }
```

Do not replace the root with `Vec<BlobRef>`. A bare list cannot describe the
roles, names, ordering, media types, or relationships among several pieces of
one semantic result.

The root CAS value may instead be a structured manifest containing any number
of child references:

```json
{
  "summary": { "status": "succeeded" },
  "output": [
    { "stream": "stdout", "text": "readable output" },
    {
      "stream": "stdout",
      "blobRef": "sha256:...",
      "mediaType": "application/octet-stream"
    }
  ],
  "artifacts": [
    {
      "name": "report.png",
      "blobRef": "sha256:...",
      "mediaType": "image/png"
    }
  ]
}
```

The cardinalities are therefore:

- one Promise has at most one root payload ref or one root error ref;
- one `await` observes up to the bounded maximum of 32 Promises and may
  aggregate that many root values; and
- one root value may describe arbitrarily many child blob/media/artifact
  references.

Producers that create a root manifest must record CAS edges from the root to
its child blobs so graph-based retention preserves the complete value. The
engine and `await` do not interpret those domain-specific relationships.

## Generic Await Materialization

The deterministic engine owns the wake claim and Promise snapshot but cannot
read CAS. The Temporal workflow must not read every payload into workflow
history either: subagent output and bounded sets of job results can be large.

Add one storage-backed activity, conceptually:

```text
materialize_await_result(AwaitSnapshotWithRefs) -> BlobRef
```

The request contains only bounded metadata and CAS references:

```text
AwaitSnapshotWithRefs
  outcome
  results[]
    promise_id
    status
    payload_ref?
    error_ref?
```

The activity:

1. reads each referenced root value directly from CAS;
2. embeds valid JSON as a JSON value;
3. embeds other valid UTF-8 as a JSON string;
4. represents an opaque/non-UTF-8 root as a blob reference rather than
   Base64-inlining it;
5. constructs the canonical total result in requested Promise order;
6. stores that result in CAS; and
7. returns only its content-addressed ref to workflow history.

The same returned ref becomes both:

- `ToolCallResult.output_ref`, the structured output of `await`; and
- the content ref of the single model-visible `ToolResult` associated with the
  original `await` call id.

This matches ordinary tool behavior and removes the current distinction
between a machine-readable ref-only output and a human summary that omits the
actual result.

The activity is generic. It knows JSON, UTF-8, opaque bytes, Promise status,
and CAS; it knows nothing about environment jobs, subagents, workflow tools,
receipts, or artifacts. It dereferences exactly each Promise's root
`payload_ref` or `error_ref`. It does not recursively fetch child `blobRef`
fields found inside a JSON value.

Content-addressed writes make activity retry idempotent. The bounded request
and single-ref response keep large result bytes out of Temporal history.

## Mailbox Messages And Detached Promises

Mailbox delivery remains distinct from Promise results.

When `await { mailbox: true }` wakes for a buffered message, the genuine
inbound message remains a user-role context entry and is consumed according to
P94. The `await` tool result reports `outcome: "mailbox_message"` plus the
current Promise snapshot. Folding real caller or Fleet mailbox input into a
tool result would lose its provenance and invert the same distinction P111 is
repairing.

Detached Promise follow-ups are also outside P111. A detached Promise that
settles without an active await deliberately requests a later run, and the
current implementation uses submitted input to trigger that run. Replacing
that input with a typed system-trigger context kind is a separate design; it
must not keep explicit `await` payloads mislabeled in the meantime.

## Joined Workflow Tools

P106 Joined completion already has the correct result insertion semantics and
does not change:

1. the workflow-tool invocation creates one internal, runtime-owned reply
   Promise;
2. the original workflow-tool call remains pending and the batch parks;
3. the Promise resolves or fails;
4. the engine completes the original call id; and
5. the Promise payload or error ref becomes that call's normal tool result.

Several Joined calls in one batch each receive their own result under their
own original call id. There is no model-visible Promise acknowledgement,
`await` call, aggregate wrapper, or synthetic user message.

Explicit Promises cannot reuse that exact one-to-one insertion because one
`await` may observe several Promises created by unrelated calls and sources.
Their correct equivalent is one ordered aggregate attached to the `await`
call. Do not emit several provider tool results for the same `await` call id
and do not retroactively revise the already-completed calls that created the
Promises.

## Implementation Notes

- `WorkflowActivities::materialize_await_result` performs the bounded CAS
  reads and one aggregate write; workflow history receives only the aggregate
  ref.
- `ToolBatchResumeOutput::AwaitTool` now carries only `result_ref`, which is
  used for both `ToolCallResult.output_ref` and the single ToolResult context
  entry.
- `tools::environment::jobs::normalize_job_result` is shared by direct
  `job_read` and terminal environment-job Promise production. Binary output
  segments are stored in CAS, and production roots record containment edges.
- Because the raw host DTO intentionally has no truncation flag, the semantic
  `truncated` field is derived at the model-facing boundary when the returned
  byte count reaches the requested cap.
- Fleet `agent_read` received the adjacent provenance cleanup immediately
  after P111: its structured output ref is now its sole ToolResult content ref.
- Live Temporal coverage verifies explicit Fleet and workflow-tool awaits,
  `agent_read`, a two-job environment await, readable host-bridge output,
  failed workflow Promises, and both Bound and Start Joined completion without
  Promise-derived user entries.

## Readable Environment Job Results

Promise aggregation must not contain feature-specific decoding. Environment
jobs normalize their result before resolving a Promise.

Introduce one shared model-facing job-result normalizer used by:

- direct `job_read` tool results;
- terminal environment-job Promise payloads; and
- a future Joined job-start surface.

Keep `ByteChunk`, `JobOutputChunk`, and raw `JobReadResult` below that boundary
for host transport, control-plane clients, and lossless diagnostics.

The semantic model-facing result preserves useful job facts while replacing
transport chunks with ordered output segments:

```text
ModelJobResult
  summary
  output[]
    Text   { stream, text }
    Blob   { stream, blob_ref, media_type?, byte_len? }
  output_next_seq
  truncated
  artifacts[]
```

The exact Rust names may differ, but the wire shape must be stable and shared
between direct reads and workflow completion.

The normalizer must:

1. consume chunks in observed sequence order;
2. reconstruct bytes before UTF-8 decoding so a code point split across
   transport chunks remains valid text;
3. preserve stdout/stderr identity and ordering;
4. merge adjacent text segments from the same stream;
5. preserve `outputNextSeq` as the cursor for a subsequent `after_seq` read;
6. make truncation explicit rather than presenting a bounded tail as complete;
7. preserve summaries, failures, timestamps, dependencies, handles, and
   semantically useful artifact metadata; and
8. store binary/media content in CAS and expose only typed refs instead of
   Base64 bytes.

Binary and media bodies are not inserted into the `await`, `job_read`, or
Joined tool result. Their descriptors and refs may appear in the semantic
JSON so a later explicit read/media activation feature can use them. P111 does
not add automatic media context activation.

Environment-job polling stores the normalized semantic job result as the
Promise's root `payload_ref`. Generic `await` then embeds that JSON value
without knowing that it describes a job. A future Joined job tool can return
the same root ref directly through P106.

## Failure Semantics

P111 does not change the Promise state machine.

- A successfully completed source with a semantic result resolves the Promise
  and supplies its root value.
- A source-level failure fails the Promise and supplies an error root when
  available.
- `await` itself succeeds whenever its wait condition is validly satisfied,
  even when one or more observed Promises failed or were cancelled.
- Joined preserves its existing mapping: a resolved internal Promise succeeds
  the original call; a failed internal Promise fails it; cancellation cancels
  it.

Environment-job domain policy must remain consistent between async and future
Joined forms. P111 preserves today's broad policy—successful jobs resolve and
non-success terminal jobs fail—while making the successful result readable.
Changing which job terminal statuses are structured successes is a separate
product decision.

## Durable State And Projection

No new durable Promise event vocabulary is required. `Promise::Resolved` and
`Promise::Failed` continue storing one optional root ref, and engine replay
continues rebuilding exactly the same Promise state.

Change the await-resume output carrier from the current structured-output plus
summary pair to one materialized result ref. On accepted resume the engine
records:

- `BatchResumed`;
- message-consumption events when the wake is mailbox-driven; and
- one successful `Tool::CallCompleted` for `await`, whose `output_ref` and
  model-visible `ToolResult` content ref are the materialized aggregate.

Delete the Promise-payload `Message { role: User }` entries and their preview
labels. API projection already derives displayed tool-call output from
`ToolResult` context, so clients should show the complete aggregate inside the
`await` tool card without an await-specific UI path or wire-schema change.

Previously written greenfield session logs and running workflow histories need
not remain replayable. P111 does not add a Temporal patch marker, dual resume
shape, transcript rewrite, or API compatibility alias.

## Adjacent Cleanup

Completed alongside P111's follow-up: Fleet `agent_read` now makes its bounded
structured `AgentReadOutput` both the call output and sole ToolResult. Retrieved
child transcripts remain inspection data under that result and no longer
create user-role entries. This stays independent from P111's generic await
activity, which has no Fleet knowledge.

## Non-Goals

P111 does not:

- add the Joined environment-job tool proposed in the later usability note;
- choose between one-job and aggregate-group Joined job semantics;
- add environment-level non-secret variables;
- change Promise creation, scoping, detach, cancellation, deadlines, or wake
  precedence;
- make Promise payloads a vector of untyped refs;
- recursively materialize manifest child blobs;
- inject returned media into model context automatically;
- change raw host/control-plane job DTOs; or
- introduce job-specific logic into `harness` or the generic `await`
  materializer.

## Implementation Plan

1. **Canonical await result types**
   - Define the materializer request and canonical model-visible output.
   - Preserve requested Promise order and the bounded 32-Promise limit.
   - Define JSON, UTF-8, missing-root, and opaque-root materialization rules.
2. **Storage-backed materialization activity**
   - Read Promise root blobs and write the aggregate entirely inside the
     activity.
   - Return only the aggregate `BlobRef` to the workflow.
   - Register the activity on the core worker and add idempotent unit coverage.
3. **Engine await insertion**
   - Replace the summary/output ref pair with one result ref.
   - Complete `await` with one `ToolResult` pointing at that ref.
   - Delete `await_user_message` and Promise-derived user context entries.
   - Keep buffered mailbox inputs as their original message entries.
4. **Shared environment-job normalizer**
   - Add the semantic model-facing job result and byte-stream normalizer.
   - Use it for `job_read` visible output and environment-job terminal Promise
     payload creation.
   - Persist binary segments as CAS blobs and retain refs/metadata without
     inlining their bodies.
5. **Projection and scripted consumers**
   - Verify the ordinary API projection displays the aggregate as the await
     tool output and emits no synthetic user item.
   - Update Fleet, environment-job, and workflow-plugin scripted LLMs to read
     results solely from tool output.
6. **Documentation cleanup**
   - Update P92/P94's explicit-await transcript description.
   - Preserve P106's direct Joined mapping and clarify its contrast with the
     explicit aggregate.
   - Mark readable job output as absorbed here while retaining the later
     document's Joined-job and variable proposals.

## Regression Coverage

### Generic await

- one resolved JSON Promise is embedded as an object in one tool result;
- one resolved text Promise is embedded as a string;
- several Promises preserve requested order and each distinct value;
- `any` and timeout include pending entries alongside terminal values;
- failed, cancelled, and missing-payload Promises use the canonical total
  representation;
- an opaque non-UTF-8 root is represented by ref without Base64 inlining;
- no resolved or failed Promise creates a user-role context entry;
- a mailbox wake still delivers genuine buffered user/Fleet messages as
  messages; and
- OpenAI Responses and Anthropic Messages each materialize exactly one
  provider-native tool result for the `await` call id.

### Environment jobs

- stdout-only UTF-8 is readable and contains no Base64 chunk field;
- interleaved stdout/stderr preserves stream identity and order;
- a UTF-8 code point split across transport chunks decodes correctly;
- adjacent same-stream chunks merge without losing cursor position;
- binary output becomes a CAS ref descriptor rather than inline Base64;
- truncation and `outputNextSeq` are explicit;
- direct `job_read` and awaited terminal completion expose the same semantic
  result shape; and
- several jobs started together resolve to several Promise entries inside one
  `await` tool result.

### Joined workflow tools

- a resolved Joined Promise still becomes the original workflow-tool call's
  normal result with the exact root payload ref;
- a failed Joined Promise still fails the original call;
- several Joined calls retain one result per original call id; and
- Joined creates neither a model-visible Promise, an `await` aggregate, nor a
  user-role payload entry.

## Done When

- explicit `await` produces one complete tool result and no Promise-derived
  user messages;
- subagent, workflow-tool, timer, and environment-job Promises share the same
  generic aggregate contract;
- text-producing jobs are readable through both `job_read` and `await` without
  leaking Base64 transport chunks;
- binary/media results remain out of model context and are represented by
  typed CAS refs;
- one Promise retains one root ref while structured roots can retain any
  number of child refs; and
- Joined workflow tools retain their existing direct original-call result
  semantics.
