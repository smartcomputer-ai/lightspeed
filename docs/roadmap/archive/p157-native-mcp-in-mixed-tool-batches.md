# P157 — Native MCP in Mixed Tool Batches

**Status**

- Proposed 2026-09-03 after diagnosing a production run of
  `bot:v1:requirements-engineer-and-ux-g3` on the hosted deployment. Two injected Notion MCP
  calls failed when the model emitted them beside a managed workflow tool in
  one turn, then succeeded unchanged when retried alone. Later native MCP
  calls also succeeded in a parallel-only batch.
- Implemented 2026-09-03 (uncommitted). Mixed batches now execute as one
  logical batch in one activity; see "Design" below for the shape and the
  invariants it relies on.

## Problem

A model turn may mix an admitted workflow tool with injected native MCP
calls. The engine accepts the calls and the provider-facing toolset exposes
all of them, but the execution boundary cannot run that combination.

Any batch containing a workflow tool or `await` takes the batch-unit path.
That path sends the original `ToolInvocationBatchRequest` to
`SessionTools::invoke_batch`, where ordinary calls fall through to the local
inline tool runtime. Injected MCP names are not local built-ins, so the calls
fail with:

```text
unsupported tool capability: unknown tool: mcp_notion__notion-fetch
```

Native MCP routing is currently materialized only on the ordinary per-call
path. There, `remote_mcp_call_runtime` resolves the injected name and adds a
`RemoteMcpCallRuntime` to `ToolInvocationCallRequest`; the worker activity
then dispatches through `NativeMcpExecutor`. The batch-unit request carries
no equivalent per-call routing facts, and its activity never consults the
native MCP executor.

This is not a Notion, authentication, rate-limit, or general parallelism
failure. In the production session:

- the first batch contained `bot_event_read` and two
  `mcp_notion__notion-fetch` calls;
- both MCP calls failed within the same append with the identical local
  `unknown tool` error;
- the same fetch argument blobs succeeded on the next two turns when each
  call used the per-call path;
- three later Notion searches completed successfully in one parallel native
  MCP batch.

The user-visible symptom therefore depends on the sibling call: native MCP
works until any call in the same model turn requires batch-unit execution.

## Design

The fix keeps one logical batch and one batch activity. Nothing is split into
separate activities; each call is dispatched by kind inside the activity.

- **Routing facts are engine-owned and per-dispatch.** `ToolInvocationRequest`
  carries `remote_mcp: Option<RemoteMcpCallRuntime>`. The engine's batch
  request builder (`next_tool_batch_request`) materializes it for every call
  from the toolset and the run's approval records, exactly as it already
  materializes workflow-tool facts. Both execution paths read that one field;
  the workflow no longer resolves routing itself. The request type is transient
  activity input (durable state holds `ObservedToolCall`), so transport
  configuration stays out of session state.
- **The activity wrapper owns native MCP dispatch on both paths.** The tool
  runtime behind `CoreAgentTools` (`SessionTools`, or a fake in tests) never
  sees a native MCP call. For a batch-unit dispatch the wrapper hands the
  runtime the batch minus its MCP calls, executes the MCP calls itself through
  the helper shared with the per-call activity (same target validation,
  approval gate, asset storage, result projection, remote operation deadline,
  and failure conversion), and merges the results back in batch order. MCP
  calls run concurrently under the existing per-batch concurrency cap, so
  several remote calls cannot exhaust the batch activity's budget.
- **Approvals follow the per-call contract.** `ToolBatchOutcome` gains
  `awaiting_approval { completed_results, approvals }`. Every ungated call
  (including the workflow tool) completes and appends durably in the same
  action that parks the run; the gated calls stay pending. The engine's
  `request_native_mcp_approvals` records N approval requests and one park, so
  a dispatch parks exactly once however many calls are gated, and the run
  unparks only when every decision has landed. The re-dispatch is the same
  batch id carrying only the still-pending calls with each decision pinned on
  its routing facts. If no workflow tool remains pending, that re-dispatch
  takes the ordinary per-call path.
- **`await` never defers while a decision is outstanding.** A deferral the
  runtime reports beside a gated MCP call is discarded; the await stays
  pending and runs again with the gated calls after the decisions.
- **Promise-slot invariant.** A batch-unit dispatch mints promise ids from
  the batch base across its calls, while a per-call re-dispatch uses
  `base + index` within the re-emitted request. This is safe only because
  native MCP calls never mint promises and every promise-minting call
  completes in the first dispatch. Any future call kind that can both mint a
  promise and stay pending across a park must revisit this rule.

Guidance: injected MCP names are not toolset keys, so the per-call path treats
them as exclusive today. Making them parallel-safe is a reasonable follow-up,
but two gated calls in one per-call group would then need the multi-call park
introduced here on that path as well.

Until the change is deployed, disabling provider parallel tool use or
instructing a profile not to combine managed workflow tools with MCP calls
avoids the bad path, but neither is a correctness solution.

## Tests

- Engine: the batch request carries routing facts for injected calls; an
  `awaiting_approval` outcome completes ungated calls and parks once for two
  gated calls; decisions unpark only as a set and the re-dispatch carries only
  pending calls with decisions pinned; the park rejects empty, unknown, and
  duplicate call sets; `call_request` forwards the call's routing facts.
- Activity wrapper: mixed batches reach the runtime minus their MCP calls and
  park once for every gated call; decided calls complete inside the batch in
  batch order (rejection is a tool error, approval reaches the executor);
  an `await` stays pending beside a gated call; deferred batches carry MCP
  results; runtime failures fail only runtime calls; a worker without a native
  runtime fails the MCP call, not the batch.
- Live (`mcp_live`): one turn holding an `await` and an approval-gated injected
  call through the hosted runtime parks once, completes after approval, and
  never reports an unknown tool.

## Acceptance

- A single model turn containing one managed workflow tool and two injected
  native MCP calls completes all three calls through their intended runtimes.
- The MCP calls receive the same target, authorization, allowlist, approval,
  timeout, asset handling, and model-visible result behavior as the existing
  per-call path.
- A real MCP failure is reported as that server/transport failure; an
  injected MCP name never reaches the inline runtime as an unknown tool.
- Existing workflow-tool-only, `await`, native-MCP-only parallel, approval,
  cancellation, and deterministic replay coverage remains green.

## Non-Goals

- Changing the provider's decision to emit parallel tool calls.
- Making Notion-specific routing or retry policy.
- Changing workflow-tool promise semantics merely to work around the missing
  native MCP dispatch case.
