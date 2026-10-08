# Lightspeed Workflow Contract

Generated from the Rust contract types in `harness` and `temporal-workflow`.
Type shapes live in `workflow.schema.json`; constants and known-answer vectors
live in `workflow.json`. Regenerate both artifacts and this reference with:

```console
cargo run -p temporal-workflow --bin export-workflow-contract
```

## Transport

The Temporal signal `deliver_emission` carries emission facts in both directions.
Its sole argument is an `EmissionEnvelope`: a deterministic
`emission_id`, a `producer`, and a tagged `body`. `AgentSessionWorkflow` and
`EnvironmentJobWorkflow` handle this signal; receivers register the same
handler. Signal stable workflow ids, never run ids, so delivery survives
continue-as-new.

Delivery is at least once with bounded retries. A receiver must persistently
deduplicate by `emission_id` and treat a duplicate as a no-op. Tool-invocation
emissions reuse their invocation id as the emission id.

## Envelope bodies

- `run_terminal`: a session run reached a terminal status. Core sends this
  only to the immutable lifecycle controller and only for a run started with
  terminal notification. The token is opaque controller correlation state.
- `source_resolution`: a workflow resolved, failed, or cancelled one keyed
  Promise. The session accepts it only when the workflow producer id exactly
  matches the Promise source recorded at admission.
- `tool_invocation`: a pushed invocation for a bound receiver. Its
  `holder_workflow_id` is the session endpoint to signal with replies; do not
  reconstruct that id. Model arguments remain in CAS at `arguments_ref`.
- `invocation_cancellation`: a best-effort notice that one completion Promise
  is already cancelled. Stop the corresponding domain work when practical;
  a later reply is ignored because the Promise is terminal.

## Producer authorization

Producer identity is authority, not metadata. Session emissions carry the
universe, session id, and exact producing log sequence. Workflow emissions
carry the universe and stable producer workflow id. A holder rejects a source
resolution whose workflow id differs from the immutable Promise source.

## Push, pull, and lifecycle gates

Push dispatch controls delivery, not completion semantics: bound workflow
tools may be pushed to their receiver or pulled from the session log. A
lifecycle controller additionally gates terminal routing and managed-session
lifecycle. Tool receivers cannot branch session state by fabricating a
terminal and are not granted lifecycle authority. A self-receiver must answer
within the session's receiver deadline so the holder cannot deadlock on itself.

## Replies and keyed completion

The reserved completion key is `reply`. Joined tools expose exactly that
one receiver-visible Promise. A receiver sends a `source_resolution` envelope
to the invocation's `holder_workflow_id`; its producer workflow id must be the
workflow execution authorized by the binding. Derived ids are deterministic,
and the known-answer vectors in `workflow.json` define the cross-language hash
framing.

## Start-on-call workflows

Start-on-call bindings launch a workflow with `WorkflowToolStartArgs`. The
execution resolves its keyed completions through the same `deliver_emission` envelope
transport. On ambiguous start recovery, query `workflow_tool_recovery` and consume a
`WorkflowToolRecoveryResult`. Recipes use format `1` and are
fingerprinted over their exact raw bytes; canonical fingerprints begin with
`wtr:sha256:`. The execution producer kind is `workflow_tool.execution`.

## Code tool invocation scopes

A trusted execution host calls the session's `open_code_tool_scope` Update with
`OpenCodeToolScopeRequest`. The parent must be an admitted, pending joined
workflow-tool invocation. The request's allowlist and budgets only narrow the
session's existing capabilities; the session resolves and pins callable bindings.

`invoke_code_tool` accepts `InvokeCodeToolRequest`: the execution and request ids, an
opaque admitted binding id, and a CAS reference to arguments. The Update waits
for the session to commit the result and its effects. Calls can overlap while
the outer joined invocation remains parked. Retrying an identical request joins
the original call or retrieves its result; conflicting identity reuse is rejected.

`close_code_tool_scope` accepts `CloseCodeToolScopeRequest` and closes new admission.
`cancel_pending` requests cleanup of admitted operations, without undoing
completed effects. Dropping an Update client wait does not cancel the operation.
Read `code_tool_scope_report` with `CodeToolScopeReportRequest` for a fresh authoritative
snapshot, including results that finish after scope closure. Reports contain
result references and attachments, never raw session effects or model context.

Scope operations return `Result<CodeToolScopeReport, CodeToolRejection>`;
invocation returns `Result<CodeToolCallOutcome, CodeToolRejection>`. Serde
encodes these as a single-key object containing `Ok` or `Err`. Admission rejection
is distinct from a successfully admitted tool returning a failed outcome.

## Schema inventory

The schema bundle contains 71 definitions. Its public roots
are: EmissionEnvelope, WorkflowToolStartArgs, WorkflowToolRecoveryResult, WorkflowToolRecipeV1, ConversationStart, ChannelDeliveryCommand, ChannelDeliveryResult, PrepareChannelMediaInput, PrepareChannelMediaResult, TranscriptionWorkflowArgs, TranscriptionSnapshot, TranscriptionActivityResult, OpenCodeToolScopeRequest, InvokeCodeToolRequest, CloseCodeToolScopeRequest, CodeToolScopeReportRequest, CodeToolScopeReport, CodeToolCallOutcome, CodeToolRejection, CodeExecutionDescriptor, CodeExecutionSnapshot, CodeRunActivityResult.
