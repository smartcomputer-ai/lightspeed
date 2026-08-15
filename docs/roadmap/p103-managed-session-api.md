# P103: Managed Session API

**Status**
- Follow-on [P106](p106-joined-workflow-tools.md) proposes the next managed
  declaration version: explicit bound pull/push dispatch plus Joined
  single-result completion. P103 remains the implemented P100b wire baseline.
- Implemented 2026-07-28.
- Expanded 2026-07-28 from the initial bound + Accepted slice to the complete
  P100/P100b managed-session surface: bound and start-on-call targets,
  Accepted and keyed-Promise completion, reply schemas, deadlines, all three
  completion-key sources, and opaque CAS-backed workflow-start recipes.
- The wire DTO expresses a promise-bearing bound receiver equal to the
  lifecycle controller. P100b B7 now admits that topology only with a non-zero
  hard deadline and proves the controller can resolve the pushed invocation
  before consuming the same run's terminal notification.
- Builds on P100/P100b's managed-session admission, workflow-backed tools,
  keyed Promises, generic workflow starts, cancellation, and run-terminal
  emissions.

## Goal

Let another Temporal workflow, or any other universe-authenticated trusted
caller, create and drive a managed session through the normal JSON-RPC API.
A Temporal workflow calls the API from an activity; workflow code itself does
not perform network I/O.

P103 adds one method to the existing main API, not a second control-plane
endpoint. The gateway already resolves every request to one universe. Under
the current coarse authorization model, a universe API key is trusted to
manage that universe; a future finer-grained capability model may gate the
method without changing its wire shape.

## API Changes

### Add `session/managed/start`

Add `ManagedSessionStartParams`, with a required creation-only
`workflowTools` document:

```text
WorkflowEndpointInput = {
  workflowId,
  workflowKind
}

ManagedSessionStartParams = {
  sessionId?,
  displayName?,
  config?,
  profile?,
  workflowTools: {
    version: 1,
    lifecycleController?: WorkflowEndpointInput,
    tools: [
      {
        definition: {
          toolId,
          revision,
          semanticType,
          tool: {
            name,
            kind: {
              type: "function",
              descriptionRef?,
              inputSchemaRef,
              outputSchemaRef?,
              strict?,
              providerOptionsRef?
            },
            parallelism?: "exclusive" | "parallelSafe"
          }
        },
        target:
          | { type: "bound", receiver: WorkflowEndpointInput }
          | {
              type: "start",
              start: {
                recipeFormat,
                revision,
                recipeRef,
                recipeFingerprint
              }
            },
        completion:
          | { type: "accepted" }
          | {
              type: "promises",
              replySchemaRef?,
              deadlineAfterMs?,
              maxPromises,
              keySource:
                | { type: "reply" }
                | { type: "stringArray", pointer }
                | { type: "arrayIndices", pointer, prefix }
            }
      }
    ]
  }
}
```

The required document must name a lifecycle controller or declare at least
one workflow tool. An empty document is rejected so the managed method cannot
collapse to ordinary session creation.

The nesting deliberately tracks the existing internal tool vocabulary:

```text
WorkflowToolDeclarationInput
  -> WorkflowToolDeclaration
       definition: WorkflowToolDefinition
         tool: ToolSpec
           name
           kind: ToolKind::Function(FunctionToolSpec)
           parallelism
           target_requirement: None       // supplied by the gateway
       target: Bound { receiver } | Start { start }
       completion: Accepted | Promises { ... }
```

The function fields and camel-case enum values match the existing
`ToolKindView::Function` and `ToolParallelismView` API vocabulary. `toolId`
is the durable workflow-tool identity copied into `workflowToolEmitted`;
`tool.name` is the distinct canonical toolset, provider-visible, and dispatch
name. `parallelism` defaults to `parallelSafe`, matching the current API view
default.

The gateway resolves defaults before admission, so omitted `parallelism` and
explicit `parallelSafe` produce the same durable definition. It preserves the
existing optionality of document refs and `strict` because those fields
participate in the workflow-tool binding fingerprint. Admission keeps the
independent collision checks for workflow `toolId` and canonical `tool.name`.

The API exposes every target and completion shape admitted by P100/P100b:

- `bound + accepted` records a durable fact for pull reconciliation at the
  run boundary;
- `bound + promises` pushes the invocation to the fixed receiver and creates
  the declaratively keyed Promise set;
- `start + promises` starts the admitted recipe once per invocation and
  creates the same keyed Promise set.

`start + accepted` remains rejected because fire-and-forget workflow start is
deferred in P100b. A promise-bearing bound tool may target the session's own
lifecycle controller under P100b's self-receiver progress contract: its hard
deadline must be present and non-zero, and the controller must process the
invocation independently of run-terminal handling without re-entering the
emitting session to compute the reply.

Callers put start-recipe bytes through `blobs/put` and supply their opaque
`recipeRef`, format, revision, and fingerprint. The current Temporal adapter
supports recipe format 1 (`workflowType` + `taskQueue`); keeping the wire
shape opaque avoids baking that adapter recipe into the substrate-neutral API.

`replySchemaRef` validates each keyed resolution payload. `deadlineAfterMs`
sets the relative hard deadline for each Promise. `keySource` supports the
single reserved `reply` key, a validated string array, or stable indices over
a validated argument array. The existing admission caps and validation laws
apply unchanged.

Callers never supply `targetRequirement`; the gateway fixes it to `None` so
workflow routing cannot enter model arguments. An optional `outputSchemaRef`
remains ordinary function metadata and is independent from a Promise
`replySchemaRef`.

The document remains separate from `SessionConfig`:

- it is accepted only on initial session creation;
- it is recorded atomically as the existing immutable managed-session fact;
- `session/config/put` cannot add, replace, or retarget it;
- retrying the same session id with the same managed-creation fingerprint
  returns the existing session;
- a conflicting declaration is rejected, and an existing standalone session
  cannot be upgraded to managed.

`session/managed/start` returns the existing `SessionStartResponse`.
Ordinary `session/start` remains unchanged and does not accept
`workflowTools`; callers must retry managed creation through the managed
method.

### Extend `session/runs/start`

Add an optional controller notification to `RunStartParams`:

```text
notifyOnTerminal?: {
  token: string
}
```

When present, the session must be managed and have a durable
`lifecycleController`. The gateway derives `holder_workflow_id` from that
controller and admits one `RunTerminalNotifyIntent`; callers never supply a
second raw workflow id at run start. The token is bounded, opaque, durable,
and delivered at least once, so the controller must handle duplicates.

The notification is optional. Non-workflow trusted code may omit it and
observe terminal state through `session/events/read` or `session/read`.

`notifyOnTerminal` participates in submission idempotency. Retrying a
client-supplied `submissionId` with a different token or with notification
presence changed is rejected rather than returning the original run.

The existing `session/runs/start` response is unchanged.

## Consuming Invocations And Results

P103 adds no second workflow-tool read or wait protocol.

For Accepted tools, after observing the run terminal the caller uses the
existing APIs:

1. paginate `session/events/read` and select `workflowToolEmitted` events for
   the exact run and admitted tool;
2. load each `argumentsRef` through `blobs/read`.

Those event views already expose the invocation identity, semantic type,
schema revision, tool id, run id, and CAS arguments reference. A dedicated
receiver-authorized projection can be exposed later only if the main API
gains receiver-scoped credentials; current universe credentials can already
read the complete session event stream.

Promise-bearing bound tools are pushed through `deliver_emission`; start
targets receive the same invocation in their deterministic start arguments.
Receivers resolve keyed Promises through the existing source-resolution
emission path, and agents use the ordinary `await`, `cancel`, and `detach`
tools. No managed-session-specific waiting method is added.

## Implementation

- Add the `session/managed/start` method and API wire DTOs for the complete
  target/completion vocabulary, plus the optional `notifyOnTerminal` run
  field; do not expose engine DTOs directly from `api`. Preserve the existing
  `ToolSpec`/`ToolKind` structure and reuse the field and enum vocabulary of
  `ToolView` wherever the concepts are identical.
- Route `session/managed/start` through the existing internal managed-session
  admission behavior. Keep `session/start` free of workflow ownership data.
- Route `session/runs/start` through the existing run request path with a
  derived `RunTerminalNotifyIntent` when `notifyOnTerminal` is present.
- Include terminal notification presence/token in retry equivalence and the
  durable run submission digest.
- Validate the same schema refs, start refs, names, collisions, completion
  keys, Promise caps, target/completion combinations, universe, and
  managed-creation fingerprint already enforced by the internal boundary.
- Regenerate `crates/api/contract`, `clients/typescript`, and
  `platform/configurator-mcp` after the wire change.

## Acceptance Criteria

1. A universe-authenticated caller can create or idempotently reopen a
   managed session containing any valid P100/P100b workflow-tool declaration.
2. Bound Accepted, bound keyed-Promise, and start-on-call keyed-Promise tools
   map to the same durable internal declarations used by trusted in-process
   callers.
3. A controller workflow can start a run with its opaque terminal token,
   receive the terminal emission, and read the run's accepted invocations
   through existing event/blob APIs; pushed and start-on-call receivers can
   resolve their keyed Promises through the ordinary emission path.
4. Ordinary `session/start` remains unchanged, and run start remains
   unchanged when `notifyOnTerminal` is absent.
5. Conflicting managed-session retries, notification-token retry mismatches,
   and notification requests without a lifecycle controller fail closed.
6. Exactly one managed-creation method is introduced: no mutable workflow-tool
   config path, raw per-run workflow destination, managed run/close method, or
   second invocation/result protocol is added.
